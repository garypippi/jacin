//! Unified popup window: Neovim's window grid with a mode-colored strip
//! beside it, and a panel with recording, the command line, messages and
//! candidates shown when needed
//!
//! Uses zwp_input_popup_surface_v2 which is automatically positioned near
//! the text cursor by the compositor.

use memmap2::MmapMut;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_v2, zwp_input_popup_surface_v2,
};

pub use super::layout::PopupContent;
use super::layout::{
    FOCUS_PANEL_ROWS, Header, Layout, MAX_GRID_ROWS, MAX_POPUP_HEIGHT, MAX_POPUP_WIDTH,
    MAX_VISIBLE_CANDIDATES, MODE_STRIP_WIDTH, MODE_STRIP_X, PADDING, REC_CIRCLE_RADIUS,
    REC_CIRCLE_TEXT_GAP, SCROLLBAR_WIDTH, SECTION_SEPARATOR_HEIGHT, TEXT_X, calculate_layout,
    format_recording_label, mode_color, rgba, scrollbar_thumb_geometry,
};
use super::text_render::{TextRenderer, copy_pixmap_to_shm, create_shm_pool, draw_border};
use super::theme::Theme;
use crate::State;
use crate::config;
use crate::neovim::Mode;
use crate::state::{StyledCell, WindowView};

/// Bytes for two ARGB buffers of `size` (double buffering)
fn pool_size((width, height): (u32, u32)) -> usize {
    (width * height * 4 * 2) as usize
}

/// Neovim UI size (columns, rows) for the text window: the text right of
/// the mode strip at the maximum popup size. Rows exceed the visible window
/// grid to leave room below the cursor for floats (nvim-cmp opens a menu
/// below only when `lines - cursor_row > min(10, entries)`). Floats are
/// clamped to `lines` by their owners, so they still fit the popup.
fn normal_ui_size(cell_width: f32, line_height: f32) -> (usize, usize) {
    let cols = ((MAX_POPUP_WIDTH as f32 - TEXT_X - PADDING) / cell_width).floor() as usize;
    // Minus padding and a panel row with its separator
    let grid_height =
        MAX_POPUP_HEIGHT as f32 - PADDING * 2.0 - line_height - SECTION_SEPARATOR_HEIGHT;
    let rows = (grid_height / line_height).floor() as usize;
    (cols.max(20), rows.max(MAX_GRID_ROWS + 1))
}

/// Largest popup (width, height) in focus mode: the screen of `cols` x
/// `rows` cells with the panel below it, and at least the normal maximum
fn focus_popup_size((cols, rows): (usize, usize), cell_width: f32, line_height: f32) -> (u32, u32) {
    let width = TEXT_X + cols as f32 * cell_width + PADDING;
    let height =
        PADDING * 2.0 + (rows + FOCUS_PANEL_ROWS) as f32 * line_height + SECTION_SEPARATOR_HEIGHT;
    // Aligned like the layout's width
    (
        ((width.ceil() as u32 + 3) & !3).max(MAX_POPUP_WIDTH),
        (height.ceil() as u32).max(MAX_POPUP_HEIGHT),
    )
}

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
    /// Neovim UI sizes (columns, rows): normal, focus mode
    ui_sizes: ((usize, usize), Option<(usize, usize)>),
    /// Largest popup in focus mode (the pool holds it too)
    focus_max: Option<(u32, u32)>,
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
}

impl UnifiedPopup {
    pub fn new(
        compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        renderer: TextRenderer,
        mut mono_renderer: TextRenderer,
        focus: &config::Focus,
    ) -> Option<Self> {
        let surfaces = Self::create_surfaces(compositor, input_method, qh);

        let cell_width = mono_renderer.measure_text(" ").max(1.0);
        let line_height = renderer.line_height().max(1.0);
        let normal = normal_ui_size(cell_width, line_height);
        let focus = focus
            .size()
            .map(|(cols, rows)| (cols.clamp(20, 500), rows.clamp(MAX_GRID_ROWS + 1, 200)));
        let focus_max = focus.map(|focus| focus_popup_size(focus, cell_width, line_height));
        let pool_bytes =
            pool_size((MAX_POPUP_WIDTH, MAX_POPUP_HEIGHT)).max(focus_max.map_or(0, pool_size));
        let (pool, pool_data) = create_shm_pool(shm, qh, pool_bytes, "ime-unified-popup")?;

        Some(Self {
            surfaces: Some(surfaces),
            compositor: compositor.clone(),
            input_method: input_method.clone(),
            pool,
            pool_data,
            ui_sizes: (normal, focus),
            focus_max,
            buffers: [None, None],
            current_buffer: 0,
            width: 200,
            height: 100,
            visible: false,
            renderer,
            mono_renderer,
            scroll_offset: 0,
            above: false,
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

    /// Neovim UI size (columns, rows) for the text window, or for the
    /// whole screen in focus mode
    pub fn ui_size(&self, focus: bool) -> (usize, usize) {
        match self.ui_sizes.1 {
            Some(size) if focus => size,
            _ => self.ui_sizes.0,
        }
    }

    /// Focus mode needs its size in the config
    pub fn has_focus_size(&self) -> bool {
        self.ui_sizes.1.is_some()
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

        let max_size = match self.focus_max {
            Some(max) if content.focus => max,
            _ => (MAX_POPUP_WIDTH, MAX_POPUP_HEIGHT),
        };
        let layout = calculate_layout(
            content,
            self.above,
            max_size,
            &mut self.renderer,
            &mut self.mono_renderer,
        );
        if (layout.width, layout.height) != (self.width, self.height) {
            log::debug!("[POPUP] Size {}x{}", layout.width, layout.height);
        }
        self.width = layout.width;
        self.height = layout.height;

        self.render(content, &layout, qh);
        self.visible = true;
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

    pub fn hide(&mut self) {
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
        if buffer_size * 2 > self.pool_data.len() {
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

        let theme = &content.theme;
        pixmap.fill(rgba(theme.bg));

        draw_border(&mut pixmap, self.width, self.height, rgba(theme.border));

        if layout.has_text {
            self.render_mode(&mut pixmap, content, layout);
            if let Some(ref view) = content.window_view {
                self.render_grid_section(&mut pixmap, view, content.mode, theme, layout);
            }
        }

        // Separator between the text and the panel (pixel-aligned:
        // fractional positions trip tiny-skia's AA hairline path)
        if let Some(sep_y) = layout.separator_y {
            fill_rect(
                &mut pixmap,
                PADDING,
                sep_y.round(),
                self.width as f32 - PADDING,
                1.0,
                rgba(theme.border),
            );
        }

        self.render_panel(&mut pixmap, content, layout);

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
        theme: &Theme,
        layout: &Layout,
    ) {
        let geo = CellGeometry {
            left: TEXT_X,
            top: layout.text_y,
            cell_width: self.mono_renderer.measure_text(" "),
            line_height: self.renderer.line_height(),
            right_edge: layout.width as f32 - PADDING,
            theme: *theme,
        };
        let block_cursor =
            view.cursor_visible && !matches!(mode, Mode::Insert | Mode::Replace | Mode::CmdLine);
        let (cursor_row, cursor_col) = view.cursor;
        let on_cursor = |row: usize, col: usize, cell: &StyledCell| {
            block_cursor
                && row == cursor_row
                && (col == cursor_col || (col == cursor_col + 1 && cell.text.is_empty()))
        };
        let text_cursor = !view.cursor_on_float;

        for (row, cells) in view.rows.iter().enumerate() {
            self.draw_grid_row(pixmap, &geo, row, 0, cells, |col, cell| {
                if text_cursor && on_cursor(row, col, cell) {
                    CellStyle::Cursor
                } else {
                    CellStyle::Normal
                }
            });
        }

        // Cursor past the end of the row (or on an empty row)
        let row_len = view.rows.get(cursor_row).map_or(0, Vec::len);
        if text_cursor && (!block_cursor || cursor_col >= row_len) {
            self.draw_cursor(pixmap, &geo, view, block_cursor);
        }

        // Floating windows on top, lowest zindex first
        for float in &view.floats {
            for (r, cells) in float.rows.iter().enumerate() {
                let row = float.row + r;
                self.draw_grid_row(pixmap, &geo, row, float.col, cells, |col, cell| {
                    if !text_cursor && on_cursor(row, col, cell) {
                        CellStyle::Cursor
                    } else {
                        CellStyle::Opaque
                    }
                });
            }
        }

        // A block cursor in a float is a cell style (float cells are not
        // trimmed); a bar goes over the float
        if !text_cursor && !block_cursor {
            self.draw_cursor(pixmap, &geo, view, false);
        }
    }

    /// Draw the cursor as a bar, or as a block on an empty cell
    fn draw_cursor(&self, pixmap: &mut Pixmap, geo: &CellGeometry, view: &WindowView, block: bool) {
        let (cursor_row, cursor_col) = view.cursor;
        let x0 = geo.x(cursor_col);
        if !view.cursor_visible || x0 >= geo.right_edge {
            return;
        }
        let (y_top, height) = (geo.y(cursor_row), geo.line_height.round());
        let theme = &geo.theme;
        let color = rgba(theme.cursor.map_or(theme.fg, |cursor| cursor.bg));
        let x1 = if block {
            geo.x(cursor_col + 1).min(geo.right_edge)
        } else {
            x0 + 2.0
        };
        fill_rect(pixmap, x0, y_top, x1, height, color);
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
            .map(|(i, cell)| cell_colors(cell, style_of(first_col + i, cell), &geo.theme))
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

    /// Render the mode strip along the text
    fn render_mode(&self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        fill_rect(
            pixmap,
            MODE_STRIP_X,
            layout.text_y.round(),
            MODE_STRIP_X + MODE_STRIP_WIDTH,
            layout.text_height.round(),
            rgba(mode_color(content.mode, content.theme.border)),
        );
    }

    /// Recording indicator: a blinking red circle and the register
    fn render_recording(&mut self, pixmap: &mut Pixmap, content: &PopupContent, y: f32) {
        let line_height = self.renderer.line_height();
        if content.rec_blink_on {
            draw_filled_circle(
                pixmap,
                PADDING + REC_CIRCLE_RADIUS,
                y + line_height * 0.5,
                REC_CIRCLE_RADIUS,
                rgba(content.theme.recording),
            );
        }
        let label = format_recording_label(&content.recording);
        self.mono_renderer.draw_text(
            pixmap,
            &label,
            PADDING + REC_CIRCLE_RADIUS * 2.0 + REC_CIRCLE_TEXT_GAP,
            y + line_height * 0.75,
            rgba(content.theme.recording),
        );
    }

    /// Render the panel: recording, header, message and candidates
    fn render_panel(&mut self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        let line_height = self.renderer.line_height();
        let msg = rgba(content.theme.msg);
        if let Some(row) = layout.rows.recording {
            self.render_recording(pixmap, content, layout.row_y(row, line_height));
        }
        if let (Some(row), Some(header)) = (layout.rows.header, &content.header) {
            let y = layout.row_y(row, line_height);
            match header {
                Header::Cmdline { text, cursor_byte } => {
                    self.render_cmdline(pixmap, text, *cursor_byte, PADDING, y, msg)
                }
                Header::ShowCmd(text) => {
                    self.mono_renderer
                        .draw_text(pixmap, text, PADDING, y + line_height * 0.75, msg)
                }
            }
        }

        if let (Some(row), Some(message)) = (layout.rows.message, &content.message) {
            let y_text = layout.row_y(row, line_height) + line_height * 0.75;
            self.renderer
                .draw_text(pixmap, message, PADDING, y_text, msg);
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
        color: Color,
    ) {
        let line_height = self.renderer.line_height();
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

    /// Candidates with the selection and a scrollbar (upwards from the text
    /// when mirrored)
    fn render_candidates(&mut self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        let line_height = self.renderer.line_height();
        let visible_count = layout.rows.visible_candidates;
        let first_y = layout.row_y(layout.rows.candidates, line_height);
        let last_y = layout.row_y(layout.rows.candidates + visible_count - 1, line_height);
        let candidates_y = first_y.min(last_y);
        let scrollbar_x = self.width as f32 - SCROLLBAR_WIDTH - 2.0;
        let theme = &content.theme;

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

            let colors = if content.selected == Some(actual_idx) {
                theme.pmenu_sel
            } else {
                theme.pmenu
            };
            let right = if layout.has_scrollbar {
                scrollbar_x - 2.0
            } else {
                self.width as f32
            };
            let left = PADDING / 2.0;
            fill_rect(pixmap, left, y_base, right, line_height, rgba(colors.bg));

            self.renderer
                .draw_text(pixmap, candidate, PADDING, y_text, rgba(colors.fg));
        }

        if layout.has_scrollbar {
            let scrollbar_height = visible_count as f32 * line_height;
            fill_rect(
                pixmap,
                scrollbar_x,
                candidates_y,
                scrollbar_x + SCROLLBAR_WIDTH,
                scrollbar_height,
                rgba(theme.scrollbar),
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
                rgba(theme.thumb),
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

/// Pixel geometry and colors of the grid area in the popup
struct CellGeometry {
    left: f32,
    top: f32,
    cell_width: f32,
    line_height: f32,
    /// Cells must end at or before this x
    right_edge: f32,
    theme: Theme,
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
fn cell_colors(cell: &StyledCell, style: CellStyle, theme: &Theme) -> (Color, Option<Color>) {
    let theme_fg = rgba(theme.fg);
    let theme_bg = rgba(theme.bg);
    // Text in the background color stays opaque
    let (r, g, b, _) = theme.bg;
    let bg_as_fg = rgba((r, g, b, 255));
    let mut fg = cell.fg.map_or(theme_fg, rgb_color);
    let mut bg = cell.bg.map(rgb_color);
    if cell.reverse {
        (fg, bg) = (bg.unwrap_or(bg_as_fg), Some(fg));
    }
    match style {
        CellStyle::Normal => (fg, bg),
        CellStyle::Cursor => match theme.cursor {
            Some(cursor) => (rgba(cursor.fg), Some(rgba(cursor.bg))),
            None => (bg.unwrap_or(bg_as_fg), Some(fg)),
        },
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
