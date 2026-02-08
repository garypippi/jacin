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

use super::text_render::{TextRenderer, copy_pixmap_to_shm, create_shm_pool, draw_border};
use crate::neovim::VisualSelection;
use crate::State;

// Colors (matching existing windows)
const BG_COLOR: (u8, u8, u8, u8) = (40, 44, 52, 240);
const TEXT_COLOR: (u8, u8, u8, u8) = (220, 223, 228, 255);
const BORDER_COLOR: (u8, u8, u8, u8) = (80, 84, 92, 255);
const SELECTED_BG: (u8, u8, u8, u8) = (61, 89, 161, 255);
const CURSOR_BG: (u8, u8, u8, u8) = (97, 175, 239, 255);
const VISUAL_BG: (u8, u8, u8, u8) = (61, 89, 161, 200);
const NUMBER_COLOR: (u8, u8, u8, u8) = (152, 195, 121, 255);
const SCROLLBAR_BG: (u8, u8, u8, u8) = (60, 64, 72, 255);
const SCROLLBAR_THUMB: (u8, u8, u8, u8) = (100, 104, 112, 255);

const PADDING: f32 = 8.0;
const MAX_VISIBLE_CANDIDATES: usize = 9;
const SCROLLBAR_WIDTH: f32 = 8.0;
const NUMBER_WIDTH: f32 = 24.0;
const SECTION_SEPARATOR_HEIGHT: f32 = 1.0;
const MAX_PREEDIT_WIDTH: f32 = 400.0;

const ICON_TEXT: &str = "邪";
const ICON_SEPARATOR_WIDTH: f32 = 1.0;
const ICON_SEPARATOR_GAP: f32 = 6.0;

/// Pool size: 600×450×4×2 bytes for double buffering (~2MB)
const POOL_SIZE: usize = 600 * 450 * 4 * 2;

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
}

impl PopupContent {
    pub fn is_empty(&self) -> bool {
        !self.ime_enabled
            && self.preedit.is_empty()
            && self.keypress.is_empty()
            && self.candidates.is_empty()
    }
}

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
        let layout = self.calculate_layout(content);
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

    /// Calculate layout dimensions and section positions
    fn calculate_layout(&mut self, content: &PopupContent) -> Layout {
        let has_preedit = !content.preedit.is_empty();
        // Hide keypress when candidates are shown
        let has_keypress = !content.keypress.is_empty() && content.candidates.is_empty();
        let has_candidates = !content.candidates.is_empty();

        let line_height = self.renderer.line_height();
        let mut y = PADDING;
        let mut max_width: f32 = 0.0;

        // Icon area width: PADDING + icon_text + gap + separator + gap
        let icon_text_width = self.renderer.measure_text(ICON_TEXT);
        let icon_area_width =
            PADDING + icon_text_width + ICON_SEPARATOR_GAP + ICON_SEPARATOR_WIDTH + ICON_SEPARATOR_GAP;

        // First row is always present (icon + optional preedit)
        let preedit_y = y;
        if has_preedit {
            let text_width = self.renderer.measure_text(&content.preedit);
            let preedit_width =
                (icon_area_width + text_width + PADDING + 4.0).min(MAX_PREEDIT_WIDTH + icon_area_width);
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
            let text_width = self.renderer.measure_text(&content.keypress);
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
                let text_width = self.renderer.measure_text(candidate);
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

    /// Render the popup content
    fn render(&mut self, content: &PopupContent, layout: &Layout, qh: &QueueHandle<State>) {
        let buffer_size = (self.width * self.height * 4) as usize;
        if buffer_size * 2 > POOL_SIZE {
            log::warn!(
                "[POPUP] Buffer too large ({}x{}), skipping render",
                self.width, self.height
            );
            return;
        }

        // Find available buffer slot
        let buffer_idx = self.find_available_buffer();
        let offset = buffer_idx * buffer_size;

        // Create pixmap
        let mut pixmap = Pixmap::new(self.width, self.height).unwrap();

        // Background
        let bg_color = Color::from_rgba8(BG_COLOR.0, BG_COLOR.1, BG_COLOR.2, BG_COLOR.3);
        pixmap.fill(bg_color);

        // Border
        let border_color =
            Color::from_rgba8(BORDER_COLOR.0, BORDER_COLOR.1, BORDER_COLOR.2, BORDER_COLOR.3);
        draw_border(&mut pixmap, self.width, self.height, border_color);

        // Render sections
        self.render_icon(&mut pixmap, layout);

        if layout.has_preedit {
            self.render_preedit_section(&mut pixmap, content, layout, layout.icon_area_width);
        }

        // Draw separator below first row if more sections follow
        if layout.has_keypress || layout.has_candidates {
            let line_height = self.renderer.line_height();
            let sep_y = layout.preedit_y + line_height;
            let border_color = Color::from_rgba8(
                BORDER_COLOR.0,
                BORDER_COLOR.1,
                BORDER_COLOR.2,
                BORDER_COLOR.3,
            );
            if let Some(rect) =
                Rect::from_xywh(PADDING, sep_y, self.width as f32 - PADDING * 2.0, 1.0)
            {
                let mut paint = Paint::default();
                paint.set_color(border_color);
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
    }

    /// Render the "邪" icon and vertical separator in the first row
    fn render_icon(&mut self, pixmap: &mut Pixmap, layout: &Layout) {
        let text_color = Color::from_rgba8(TEXT_COLOR.0, TEXT_COLOR.1, TEXT_COLOR.2, TEXT_COLOR.3);
        let border_color =
            Color::from_rgba8(BORDER_COLOR.0, BORDER_COLOR.1, BORDER_COLOR.2, BORDER_COLOR.3);
        let line_height = self.renderer.line_height();
        let y_baseline = layout.preedit_y + line_height * 0.75;

        // Draw "邪" icon
        self.renderer
            .draw_text(pixmap, ICON_TEXT, PADDING, y_baseline, text_color);

        // Draw vertical separator
        let icon_text_width = self.renderer.measure_text(ICON_TEXT);
        let sep_x = PADDING + icon_text_width + ICON_SEPARATOR_GAP;
        if let Some(rect) =
            Rect::from_xywh(sep_x, layout.preedit_y, ICON_SEPARATOR_WIDTH, line_height)
        {
            let mut paint = Paint::default();
            paint.set_color(border_color);
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
        let text_color = Color::from_rgba8(TEXT_COLOR.0, TEXT_COLOR.1, TEXT_COLOR.2, TEXT_COLOR.3);
        let cursor_bg = Color::from_rgba8(CURSOR_BG.0, CURSOR_BG.1, CURSOR_BG.2, CURSOR_BG.3);
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

        let is_normal_mode = content.vim_mode == "n"
            || content.vim_mode == "v"
            || content.vim_mode.starts_with('v');

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
        let scroll_offset = if total_text_width > visible_width {
            // Calculate offset to center cursor in visible area, with some margin
            let margin = visible_width * 0.3; // 30% margin from edges
            let cursor_rel = cursor_x - preedit_left;

            if cursor_rel < margin {
                // Cursor near start - no scroll
                0.0
            } else if cursor_rel > total_text_width - margin {
                // Cursor near end - scroll to show end
                (total_text_width - visible_width).max(0.0)
            } else {
                // Center cursor in visible area
                (cursor_rel - visible_width / 2.0).clamp(0.0, total_text_width - visible_width)
            }
        } else {
            0.0
        };

        if is_normal_mode && cursor_char_begin < chars.len() {
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
                let visual_bg =
                    Color::from_rgba8(VISUAL_BG.0, VISUAL_BG.1, VISUAL_BG.2, VISUAL_BG.3);
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
                if char_x + char_width < preedit_left
                    || char_x > layout.width as f32 - PADDING
                {
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
                if char_x + char_width < preedit_left
                    || char_x > layout.width as f32 - PADDING
                {
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
    fn render_keypress_section(&mut self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        let text_color = Color::from_rgba8(TEXT_COLOR.0, TEXT_COLOR.1, TEXT_COLOR.2, TEXT_COLOR.3);
        let line_height = self.renderer.line_height();
        let y_baseline = layout.keypress_y + line_height * 0.75;

        self.renderer
            .draw_text(pixmap, &content.keypress, PADDING, y_baseline, text_color);

        // Draw separator if candidates follow
        if layout.has_candidates {
            let sep_y = layout.keypress_y + line_height;
            let border_color = Color::from_rgba8(
                BORDER_COLOR.0,
                BORDER_COLOR.1,
                BORDER_COLOR.2,
                BORDER_COLOR.3,
            );
            if let Some(rect) =
                Rect::from_xywh(PADDING, sep_y, self.width as f32 - PADDING * 2.0, 1.0)
            {
                let mut paint = Paint::default();
                paint.set_color(border_color);
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }
    }

    /// Render candidate section with scrollbar
    fn render_candidate_section(&mut self, pixmap: &mut Pixmap, content: &PopupContent, layout: &Layout) {
        let text_color = Color::from_rgba8(TEXT_COLOR.0, TEXT_COLOR.1, TEXT_COLOR.2, TEXT_COLOR.3);
        let selected_bg =
            Color::from_rgba8(SELECTED_BG.0, SELECTED_BG.1, SELECTED_BG.2, SELECTED_BG.3);
        let number_color =
            Color::from_rgba8(NUMBER_COLOR.0, NUMBER_COLOR.1, NUMBER_COLOR.2, NUMBER_COLOR.3);
        let scrollbar_bg =
            Color::from_rgba8(SCROLLBAR_BG.0, SCROLLBAR_BG.1, SCROLLBAR_BG.2, SCROLLBAR_BG.3);
        let scrollbar_thumb = Color::from_rgba8(
            SCROLLBAR_THUMB.0,
            SCROLLBAR_THUMB.1,
            SCROLLBAR_THUMB.2,
            SCROLLBAR_THUMB.3,
        );

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
            self.renderer
                .draw_text(pixmap, candidate, PADDING + NUMBER_WIDTH, y_text, text_color);
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
            let thumb_height =
                (layout.visible_count as f32 / total_count as f32) * scrollbar_height;
            let thumb_height = thumb_height.max(20.0);
            let scroll_range = total_count - layout.visible_count;
            let thumb_y = if scroll_range > 0 {
                layout.candidates_y
                    + (self.scroll_offset as f32 / scroll_range as f32)
                        * (scrollbar_height - thumb_height)
            } else {
                layout.candidates_y
            };

            if let Some(rect) = Rect::from_xywh(scrollbar_x, thumb_y, SCROLLBAR_WIDTH, thumb_height)
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

/// Layout information for rendering
struct Layout {
    width: u32,
    height: u32,
    icon_area_width: f32,
    has_preedit: bool,
    has_keypress: bool,
    has_candidates: bool,
    preedit_y: f32,
    keypress_y: f32,
    candidates_y: f32,
    visible_count: usize,
    has_scrollbar: bool,
}
