//! Unified popup window combining preedit, keypress display, and candidates
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

use super::layout::{
    Layout, calculate_layout, mode_label, preedit_scroll_offset, rgba,
    scrollbar_thumb_geometry, BG_COLOR, BORDER_COLOR, CURSOR_BG, ICON_SEPARATOR_GAP,
    ICON_SEPARATOR_WIDTH, MAX_VISIBLE_CANDIDATES, MODE_GAP, MODE_RECORDING_COLOR, NUMBER_COLOR,
    NUMBER_WIDTH, PADDING, SCROLLBAR_BG, SCROLLBAR_THUMB, SCROLLBAR_WIDTH, SELECTED_BG,
    TEXT_COLOR, VISUAL_BG,
};
pub use super::layout::PopupContent;
use super::text_render::{TextRenderer, copy_pixmap_to_shm, create_shm_pool, draw_border};
use crate::State;
use crate::neovim::VisualSelection;

/// Pool size: 600×450×4×2 bytes for double buffering (~2MB)
const POOL_SIZE: usize = 600 * 450 * 4 * 2;

/// Double buffer state
struct Buffer {
    buffer: wl_buffer::WlBuffer,
    in_use: bool,
}

/// Unified popup window
pub struct UnifiedPopup {
    surface: wl_surface::WlSurface,
    popup_surface: zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
    pool: wl_shm_pool::WlShmPool,
    pool_data: MmapMut,
    buffers: [Option<Buffer>; 2],
    current_buffer: usize,
    width: u32,
    height: u32,
    pub visible: bool,
    renderer: TextRenderer,
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
    ) -> Option<Self> {
        // Create surface
        let surface = compositor.create_surface(qh, ());

        // Create input popup surface - compositor positions this near cursor
        let popup_surface = input_method.get_input_popup_surface(&surface, qh, ());

        // Create shm pool for double-buffered rendering
        let (pool, pool_data) = create_shm_pool(shm, qh, POOL_SIZE, "ime-unified-popup")?;

        Some(Self {
            surface,
            popup_surface,
            pool,
            pool_data,
            buffers: [None, None],
            current_buffer: 0,
            width: 200,
            height: 100,
            visible: false,
            renderer,
            scroll_offset: 0,
        })
    }

    /// Update the popup with new content
    pub fn update(&mut self, content: &PopupContent, qh: &QueueHandle<State>) {
        if content.is_empty() {
            self.hide();
            return;
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
        let layout = calculate_layout(content, &mut self.renderer);
        self.width = layout.width;
        self.height = layout.height;

        // Render
        self.render(content, &layout, qh);
        self.visible = true;
    }

    /// Hide the popup
    pub fn hide(&mut self) {
        if self.visible {
            self.surface.attach(None, 0, 0);
            self.surface.commit();
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
        self.popup_surface.destroy();
        self.surface.destroy();
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
        self.render_status_bar(&mut pixmap, content, layout);

        if layout.has_preedit {
            self.render_preedit_section(&mut pixmap, content, layout, layout.icon_area_width);
        }

        // Draw separator below first row if more sections follow
        if layout.has_keypress || layout.has_candidates {
            let line_height = self.renderer.line_height();
            let sep_y = layout.preedit_y + line_height;
            if let Some(rect) =
                Rect::from_xywh(PADDING, sep_y, self.width as f32 - PADDING * 2.0, 1.0)
            {
                let mut paint = Paint::default();
                paint.set_color(rgba(BORDER_COLOR));
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }

        if layout.has_keypress {
            self.render_keypress_section(&mut pixmap, content, layout);
        }

        if layout.has_candidates {
            self.render_candidate_section(&mut pixmap, content, layout);
        }

        // Copy to SHM buffer
        let dest = &mut self.pool_data[offset..offset + buffer_size];
        copy_pixmap_to_shm(&pixmap, dest);

        // Get or create wl_buffer for this slot
        if self.buffers[buffer_idx].is_none() {
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
            });
        } else {
            let buf = self.buffers[buffer_idx].as_mut().unwrap();
            buf.buffer.destroy();
            buf.buffer = self.pool.create_buffer(
                offset as i32,
                self.width as i32,
                self.height as i32,
                (self.width * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                buffer_idx,
            );
            buf.in_use = true;
        }

        // Attach and commit
        let buffer = &self.buffers[buffer_idx].as_ref().unwrap().buffer;
        self.surface.attach(Some(buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, self.width as i32, self.height as i32);
        self.surface.commit();

        self.current_buffer = buffer_idx;
        log::trace!(
            "[PERF] render: {:.2}ms ({}x{})",
            _perf_start.elapsed().as_secs_f64() * 1000.0,
            self.width,
            self.height
        );
    }

    /// Render mode label, recording indicator, and vertical separator in the first row
    fn render_status_bar(&mut self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        let line_height = self.renderer.line_height();
        let y_baseline = layout.preedit_y + line_height * 0.75;

        // Draw mode label
        let (mode_text, mode_color) = mode_label(&content.vim_mode);
        let mode_x = PADDING;
        self.renderer
            .draw_text(pixmap, mode_text, mode_x, y_baseline, rgba(mode_color));

        // Draw recording indicator if active
        let mode_text_width = self.renderer.measure_text(mode_text);
        let mut after_mode_x = mode_x + mode_text_width;
        if !content.recording.is_empty() {
            let rec_label = format!("REC @{}", content.recording);
            let rec_x = after_mode_x + MODE_GAP;
            self.renderer.draw_text(
                pixmap,
                &rec_label,
                rec_x,
                y_baseline,
                rgba(MODE_RECORDING_COLOR),
            );
            after_mode_x = rec_x + self.renderer.measure_text(&rec_label);
        }

        // Draw vertical separator
        let sep_x = after_mode_x + ICON_SEPARATOR_GAP;
        if let Some(rect) =
            Rect::from_xywh(sep_x, layout.preedit_y, ICON_SEPARATOR_WIDTH, line_height)
        {
            let mut paint = Paint::default();
            paint.set_color(rgba(BORDER_COLOR));
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }
    }

    /// Render preedit section with cursor
    fn render_preedit_section(
        &mut self,
        pixmap: &mut Pixmap,
        content: &PopupContent,
        layout: &Layout,
        preedit_left: f32,
    ) {
        let text_color = rgba(TEXT_COLOR);
        let cursor_bg = rgba(CURSOR_BG);
        let line_height = self.renderer.line_height();
        let y_baseline = layout.preedit_y + line_height * 0.75;

        // Convert byte offsets to character positions
        let chars: Vec<char> = content.preedit.chars().collect();
        let mut byte_to_char: Vec<usize> = Vec::with_capacity(content.preedit.len() + 1);
        for (i, c) in chars.iter().enumerate() {
            for _ in 0..c.len_utf8() {
                byte_to_char.push(i);
            }
        }
        byte_to_char.push(chars.len());

        let cursor_char_begin = byte_to_char.get(content.cursor_begin).copied().unwrap_or(0);
        let cursor_char_end = byte_to_char
            .get(content.cursor_end)
            .copied()
            .unwrap_or(chars.len());

        let is_normal_mode =
            content.vim_mode == "n" || content.vim_mode == "v" || content.vim_mode.starts_with('v');

        // Calculate character positions (absolute, starting from preedit_left)
        let mut char_x_positions: Vec<f32> = Vec::with_capacity(chars.len() + 1);
        let mut x = preedit_left;
        for c in &chars {
            char_x_positions.push(x);
            x += self.renderer.measure_text(&c.to_string());
        }
        char_x_positions.push(x);

        // Calculate total text width and visible area
        let total_text_width = x - preedit_left;
        let visible_width = layout.width as f32 - PADDING - preedit_left;

        // Calculate scroll offset to keep cursor visible
        let cursor_x = char_x_positions
            .get(cursor_char_begin)
            .copied()
            .unwrap_or(preedit_left);
        let cursor_rel = cursor_x - preedit_left;
        let scroll_offset =
            preedit_scroll_offset(total_text_width, visible_width, cursor_rel);

        if is_normal_mode && cursor_char_begin <= chars.len() {
            // Convert visual selection byte offsets to char positions
            let visual_char_range = match &content.visual_selection {
                Some(VisualSelection::Charwise { begin, end }) => {
                    let vbegin = byte_to_char.get(*begin).copied().unwrap_or(0);
                    let vend = byte_to_char.get(*end).copied().unwrap_or(chars.len());
                    Some((vbegin, vend))
                }
                None => None,
            };

            // Draw visual selection background (behind cursor)
            if let Some((vbegin, vend)) = visual_char_range {
                let visual_bg = rgba(VISUAL_BG);
                let vx_start = char_x_positions[vbegin] - scroll_offset;
                let vx_end = char_x_positions[vend.min(chars.len())] - scroll_offset;
                if let Some(rect) =
                    Rect::from_xywh(vx_start, layout.preedit_y, vx_end - vx_start, line_height)
                {
                    let mut paint = Paint::default();
                    paint.set_color(visual_bg);
                    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                }
            }

            // Block cursor (drawn on top of visual selection)
            let x_start = char_x_positions[cursor_char_begin] - scroll_offset;
            let x_end = char_x_positions[cursor_char_end.min(chars.len())] - scroll_offset;
            let cursor_width = (x_end - x_start).max(self.renderer.measure_text(" "));

            if let Some(rect) =
                Rect::from_xywh(x_start, layout.preedit_y, cursor_width, line_height)
            {
                let mut paint = Paint::default();
                paint.set_color(cursor_bg);
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }

            // Draw text - cursor chars dark, visual chars light on VISUAL_BG, others normal
            let cursor_text_color = Color::from_rgba8(40, 44, 52, 255);
            for (i, c) in chars.iter().enumerate() {
                let char_x = char_x_positions[i] - scroll_offset;
                let char_width = self.renderer.measure_text(&c.to_string());

                // Skip characters outside visible area
                if char_x + char_width < preedit_left || char_x > layout.width as f32 - PADDING {
                    continue;
                }

                let color = if i >= cursor_char_begin && i < cursor_char_end {
                    cursor_text_color
                } else {
                    text_color
                };
                self.renderer
                    .draw_text(pixmap, &c.to_string(), char_x, y_baseline, color);
            }
        } else {
            // Insert mode - draw text then line cursor
            // Draw characters individually to handle scrolling
            for (i, c) in chars.iter().enumerate() {
                let char_x = char_x_positions[i] - scroll_offset;
                let char_width = self.renderer.measure_text(&c.to_string());

                // Skip characters outside visible area
                if char_x + char_width < preedit_left || char_x > layout.width as f32 - PADDING {
                    continue;
                }

                self.renderer
                    .draw_text(pixmap, &c.to_string(), char_x, y_baseline, text_color);
            }

            // Draw line cursor
            let cursor_draw_x = cursor_x - scroll_offset;
            if cursor_draw_x >= preedit_left
                && cursor_draw_x <= layout.width as f32 - PADDING
                && let Some(rect) =
                    Rect::from_xywh(cursor_draw_x, layout.preedit_y, 2.0, line_height)
            {
                let mut paint = Paint::default();
                paint.set_color(text_color);
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }
    }

    /// Render keypress section
    fn render_keypress_section(
        &mut self,
        pixmap: &mut Pixmap,
        content: &PopupContent,
        layout: &Layout,
    ) {
        let line_height = self.renderer.line_height();
        let y_baseline = layout.keypress_y + line_height * 0.75;

        self.renderer.draw_text(
            pixmap,
            &content.keypress,
            PADDING,
            y_baseline,
            rgba(TEXT_COLOR),
        );

        // Draw separator if candidates follow
        if layout.has_candidates {
            let sep_y = layout.keypress_y + line_height;
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
