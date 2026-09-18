//! Unified popup window combining Neovim's window grid, keypress display,
//! and candidates
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
    BG_COLOR, BORDER_COLOR, CURSOR_BG, ICON_SEPARATOR_GAP, ICON_SEPARATOR_WIDTH,
    KEYPRESS_ENTRY_GAP, KEYPRESS_TEXT_COLOR, Layout, MAX_GRID_ROWS, MAX_POPUP_HEIGHT,
    MAX_POPUP_WIDTH, MAX_VISIBLE_CANDIDATES, MODE_GAP, MODE_RECORDING_COLOR, NUMBER_COLOR,
    NUMBER_WIDTH, PADDING, REC_CIRCLE_RADIUS, REC_CIRCLE_TEXT_GAP, SCROLLBAR_BG, SCROLLBAR_THUMB,
    SCROLLBAR_WIDTH, SECTION_SEPARATOR_HEIGHT, SELECTED_BG, TEXT_COLOR, calculate_layout,
    format_recording_label, mode_label, rgba, scrollbar_thumb_geometry,
};
use super::text_render::{TextRenderer, copy_pixmap_to_shm, create_shm_pool, draw_border};
use crate::State;
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
}

impl UnifiedPopup {
    /// Create a new unified popup window
    pub fn new(
        compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        renderer: TextRenderer,
        mono_renderer: TextRenderer,
    ) -> Option<Self> {
        let surfaces = Self::create_surfaces(compositor, input_method, qh);

        // Create shm pool for double-buffered rendering
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

    /// Neovim UI size (columns, rows): the window
    /// section at the maximum popup size. Rows must leave room below the
    /// cursor for floats — nvim-cmp won't open a menu of 8+ entries unless
    /// `lines - cursor_row > 10`, so sizing rows to the visible window grid
    /// alone (9) hides such menus. Floats are clamped to `lines` by their
    /// owners, so they still fit the popup.
    pub fn grid_ui_size(&mut self) -> (usize, usize) {
        let cell_width = self.mono_renderer.measure_text(" ").max(1.0);
        let cols = ((MAX_POPUP_WIDTH as f32 - PADDING * 2.0) / cell_width).floor() as usize;
        let line_height = self.renderer.line_height().max(1.0);
        // Minus padding, the keypress row and its separator
        let grid_height =
            MAX_POPUP_HEIGHT as f32 - PADDING * 2.0 - line_height - SECTION_SEPARATOR_HEIGHT;
        let rows = (grid_height / line_height).floor() as usize;
        (cols.max(20), rows.max(MAX_GRID_ROWS + 1))
    }

    /// Update the popup with new content
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

        // Adjust scroll offset to keep selection visible
        if !content.candidates.is_empty() {
            let visible_count = MAX_VISIBLE_CANDIDATES.min(content.candidates.len());
            if content.selected < self.scroll_offset {
                self.scroll_offset = content.selected;
            } else if content.selected >= self.scroll_offset + visible_count {
                self.scroll_offset = content.selected - visible_count + 1;
            }
        } else {
            self.scroll_offset = 0;
        }

        // Calculate layout and size
        let layout = calculate_layout(content, &mut self.renderer, &mut self.mono_renderer);
        self.width = layout.width;
        self.height = layout.height;

        // Render
        self.render(content, &layout, qh);
        self.visible = true;
    }

    /// Hide the popup
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

    /// Destroy the window
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

    /// Render the popup content
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

        // Find available buffer slot
        let buffer_idx = self.find_available_buffer();
        let offset = buffer_idx * buffer_size;

        // Create pixmap
        let Some(mut pixmap) = Pixmap::new(self.width, self.height) else {
            log::warn!(
                "[POPUP] Failed to allocate pixmap ({}x{}), skipping render",
                self.width,
                self.height
            );
            return;
        };

        // Background
        pixmap.fill(rgba(BG_COLOR));

        // Border
        draw_border(&mut pixmap, self.width, self.height, rgba(BORDER_COLOR));

        // Render sections
        if layout.has_window {
            let window_rows = content
                .window_view
                .as_ref()
                .map_or(1, |view| view.total_rows().max(1));
            if let Some(ref view) = content.window_view {
                self.render_grid_section(&mut pixmap, view, &content.vim_mode, layout);
            }

            // Draw separator below the window if more sections follow
            if layout.has_keypress || layout.has_candidates || layout.has_transient_message {
                let line_height = self.renderer.line_height();
                // Pixel-align 1px lines (fractional positions trip tiny-skia's AA hairline path)
                let sep_y = (layout.window_y + line_height * window_rows as f32).round();
                if let Some(rect) =
                    Rect::from_xywh(PADDING, sep_y, self.width as f32 - PADDING * 2.0, 1.0)
                {
                    let mut paint = Paint::default();
                    paint.set_color(rgba(BORDER_COLOR));
                    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                }
            }
        }

        if layout.has_keypress {
            self.render_keypress_section(&mut pixmap, content, layout);
        }

        if layout.has_candidates {
            self.render_candidate_section(&mut pixmap, content, layout);
        } else if layout.has_transient_message {
            self.render_transient_message(&mut pixmap, content, layout);
        }

        // Copy to SHM buffer
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

        // Attach and commit
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
        vim_mode: &str,
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
            view.cursor_visible && !matches!(vim_mode.chars().next(), Some('i' | 'R' | 'c'));
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

    /// Render keypress section with mode/REC icons and optional keypress text
    fn render_keypress_section(
        &mut self,
        pixmap: &mut Pixmap,
        content: &PopupContent,
        layout: &Layout,
    ) {
        let line_height = self.renderer.line_height();
        let y_baseline = layout.keypress_y + line_height * 0.75;

        // Draw mode label using monospace font
        let (mode_text, mode_color) = mode_label(&content.vim_mode);
        let mode_x = PADDING;
        self.mono_renderer
            .draw_text(pixmap, mode_text, mode_x, y_baseline, rgba(mode_color));

        // Draw recording indicator if active
        let mode_text_width = self.mono_renderer.measure_text(mode_text);
        let mut after_mode_x = mode_x + mode_text_width;
        if !content.recording.is_empty() {
            let rec_x = after_mode_x + MODE_GAP;

            // Draw red filled circle (hidden during blink-off phase)
            let circle_cy = layout.keypress_y + line_height * 0.5;
            let circle_cx = rec_x + REC_CIRCLE_RADIUS;
            if content.rec_blink_on {
                draw_filled_circle(
                    pixmap,
                    circle_cx,
                    circle_cy,
                    REC_CIRCLE_RADIUS,
                    rgba(MODE_RECORDING_COLOR),
                );
            }

            // Draw @reg text using monospace font
            let rec_label = format_recording_label(&content.recording);
            let text_x = rec_x + REC_CIRCLE_RADIUS * 2.0 + REC_CIRCLE_TEXT_GAP;
            self.mono_renderer.draw_text(
                pixmap,
                &rec_label,
                text_x,
                y_baseline,
                rgba(MODE_RECORDING_COLOR),
            );
            after_mode_x = text_x + self.mono_renderer.measure_text(&rec_label);
        }

        // Draw vertical separator
        let sep_x = (after_mode_x + ICON_SEPARATOR_GAP).round();
        if let Some(rect) =
            Rect::from_xywh(sep_x, layout.keypress_y, ICON_SEPARATOR_WIDTH, line_height)
        {
            let mut paint = Paint::default();
            paint.set_color(rgba(BORDER_COLOR));
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }

        // Draw keypress entries with gap between each (hidden when candidates are shown,
        // matching calculate_layout which excludes keypress text width)
        if !content.keypress_entries.is_empty() && !layout.has_candidates {
            if let Some(cursor_byte) = content.cmdline_cursor_pos {
                // Command-line mode: render single entry char-by-char with line cursor
                let text = &content.keypress_entries[0];
                let text_left = layout.keypress_icon_width;
                let text_color = rgba(KEYPRESS_TEXT_COLOR);

                // Build byte-to-char mapping
                let chars: Vec<char> = text.chars().collect();
                let mut byte_to_char: Vec<usize> = Vec::with_capacity(text.len() + 1);
                for (i, c) in chars.iter().enumerate() {
                    for _ in 0..c.len_utf8() {
                        byte_to_char.push(i);
                    }
                }
                byte_to_char.push(chars.len());

                let cursor_char = byte_to_char
                    .get(cursor_byte)
                    .copied()
                    .unwrap_or(chars.len());

                // Calculate character x positions
                let mut char_x_positions: Vec<f32> = Vec::with_capacity(chars.len() + 1);
                let mut x = text_left;
                for c in &chars {
                    char_x_positions.push(x);
                    x += self.mono_renderer.measure_text(&c.to_string());
                }
                char_x_positions.push(x);

                // Draw characters
                for (i, c) in chars.iter().enumerate() {
                    let char_x = char_x_positions[i];
                    self.mono_renderer.draw_text(
                        pixmap,
                        &c.to_string(),
                        char_x,
                        y_baseline,
                        text_color,
                    );
                }

                // Draw line cursor (2px vertical line)
                let cursor_x = char_x_positions
                    .get(cursor_char)
                    .copied()
                    .unwrap_or(text_left);
                if let Some(rect) = Rect::from_xywh(cursor_x, layout.keypress_y, 2.0, line_height) {
                    let mut paint = Paint::default();
                    paint.set_color(text_color);
                    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                }
            } else {
                // Normal keypress display: render entries with gaps
                let mut text_x = layout.keypress_icon_width;
                for (i, entry) in content.keypress_entries.iter().enumerate() {
                    if i > 0 {
                        text_x += KEYPRESS_ENTRY_GAP;
                    }
                    self.mono_renderer.draw_text(
                        pixmap,
                        entry,
                        text_x,
                        y_baseline,
                        rgba(KEYPRESS_TEXT_COLOR),
                    );
                    text_x += self.mono_renderer.measure_text(entry);
                }
            }
        }

        // Draw separator if candidates follow
        if layout.has_candidates {
            let sep_y = (layout.keypress_y + line_height).round();
            if let Some(rect) =
                Rect::from_xywh(PADDING, sep_y, self.width as f32 - PADDING * 2.0, 1.0)
            {
                let mut paint = Paint::default();
                paint.set_color(rgba(BORDER_COLOR));
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }
    }

    /// Render candidate section with scrollbar
    fn render_candidate_section(
        &mut self,
        pixmap: &mut Pixmap,
        content: &PopupContent,
        layout: &Layout,
    ) {
        let text_color = rgba(TEXT_COLOR);
        let selected_bg = rgba(SELECTED_BG);
        let number_color = rgba(NUMBER_COLOR);
        let scrollbar_bg = rgba(SCROLLBAR_BG);
        let scrollbar_thumb = rgba(SCROLLBAR_THUMB);

        let line_height = self.renderer.line_height();
        let total_count = content.candidates.len();

        // Render visible candidates
        for (visible_idx, candidate) in content
            .candidates
            .iter()
            .skip(self.scroll_offset)
            .take(layout.visible_count)
            .enumerate()
        {
            let actual_idx = self.scroll_offset + visible_idx;
            let y_base = layout.candidates_y + (visible_idx as f32 * line_height);
            let y_text = y_base + line_height * 0.75;

            // Draw selection highlight
            if actual_idx == content.selected {
                let highlight_width = if layout.has_scrollbar {
                    self.width as f32 - SCROLLBAR_WIDTH - 4.0
                } else {
                    self.width as f32
                };
                if let Some(rect) = Rect::from_xywh(0.0, y_base, highlight_width, line_height) {
                    let mut paint = Paint::default();
                    paint.set_color(selected_bg);
                    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                }
            }

            // Draw number
            let number = format!("{}.", actual_idx + 1);
            self.renderer
                .draw_text(pixmap, &number, PADDING, y_text, number_color);

            // Draw candidate text
            self.renderer.draw_text(
                pixmap,
                candidate,
                PADDING + NUMBER_WIDTH,
                y_text,
                text_color,
            );
        }

        // Draw scrollbar if needed
        if layout.has_scrollbar {
            let scrollbar_x = self.width as f32 - SCROLLBAR_WIDTH - 2.0;
            let scrollbar_height = layout.visible_count as f32 * line_height;

            // Scrollbar track
            if let Some(rect) = Rect::from_xywh(
                scrollbar_x,
                layout.candidates_y,
                SCROLLBAR_WIDTH,
                scrollbar_height,
            ) {
                let mut paint = Paint::default();
                paint.set_color(scrollbar_bg);
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }

            // Scrollbar thumb
            let thumb = scrollbar_thumb_geometry(
                layout.visible_count,
                total_count,
                scrollbar_height,
                self.scroll_offset,
                layout.candidates_y,
            );

            if let Some(rect) = Rect::from_xywh(scrollbar_x, thumb.y, SCROLLBAR_WIDTH, thumb.height)
            {
                let mut paint = Paint::default();
                paint.set_color(scrollbar_thumb);
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }
    }

    /// Render a transient message in the candidate area
    fn render_transient_message(
        &mut self,
        pixmap: &mut Pixmap,
        content: &PopupContent,
        layout: &Layout,
    ) {
        if let Some(ref msg) = content.transient_message {
            let line_height = self.renderer.line_height();
            let y_text = layout.candidates_y + line_height * 0.75;
            self.renderer
                .draw_text(pixmap, msg, PADDING, y_text, rgba(TEXT_COLOR));
        }
    }

    /// Find an available buffer slot
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
