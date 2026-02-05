//! Keypress display window using Wayland input-method popup surface protocol
//!
//! Shows accumulated key sequences near the cursor for visual feedback during
//! Vim-style input operations.

use memmap2::MmapMut;
use std::os::fd::AsFd;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_v2, zwp_input_popup_surface_v2,
};

use super::text_render::TextRenderer;
use crate::State;

/// Keypress display window
pub struct KeypressWindow {
    surface: wl_surface::WlSurface,
    popup_surface: zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
    pool: wl_shm_pool::WlShmPool,
    pool_data: MmapMut,
    buffer: Option<wl_buffer::WlBuffer>,
    width: u32,
    height: u32,
    pub visible: bool,
    renderer: TextRenderer,
}

impl KeypressWindow {
    /// Create a new keypress display window
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

        // Create shm pool for single buffer (simpler than candidate window)
        // Size: 200x40 ARGB should be plenty for key sequences
        let pool_size = 200 * 40 * 4;
        let (pool, pool_data) = create_shm_pool(shm, qh, pool_size)?;

        Some(Self {
            surface,
            popup_surface,
            pool,
            pool_data,
            buffer: None,
            width: 100,
            height: 30,
            visible: false,
            renderer,
        })
    }

    /// Maximum width for keypress window (to prevent buffer overflow)
    const MAX_WIDTH: u32 = 180;

    /// Show the keypress display with given key sequence
    pub fn show(&mut self, keys: &str, qh: &QueueHandle<State>) {
        if keys.is_empty() {
            self.hide();
            return;
        }

        // Truncate long key sequences to prevent buffer overflow
        // Show only the last N characters that fit in MAX_WIDTH
        let padding = 8.0;
        let max_text_width = Self::MAX_WIDTH as f32 - padding * 2.0;

        let display_keys = if self.renderer.measure_text(keys) > max_text_width {
            // Find a suffix that fits
            let chars: Vec<char> = keys.chars().collect();
            let mut start = chars.len();
            loop {
                if start == 0 {
                    break;
                }
                start -= 1;
                let suffix: String = chars[start..].iter().collect();
                if self.renderer.measure_text(&suffix) <= max_text_width {
                    break;
                }
            }
            let suffix: String = chars[start..].iter().collect();
            suffix
        } else {
            keys.to_string()
        };

        // Calculate required size
        let text_width = self.renderer.measure_text(&display_keys);
        self.width = ((text_width + padding * 2.0).ceil() as u32).clamp(40, Self::MAX_WIDTH);
        self.height = (self.renderer.line_height() + padding * 2.0).ceil() as u32;

        // Align to 4 bytes for wl_shm
        self.width = (self.width + 3) & !3;

        // Render
        self.render(&display_keys, qh);
        self.visible = true;
    }

    /// Hide the keypress display
    pub fn hide(&mut self) {
        if self.visible {
            self.surface.attach(None, 0, 0);
            self.surface.commit();
            self.visible = false;
        }
    }

    /// Render key sequence to buffer
    fn render(&mut self, keys: &str, qh: &QueueHandle<State>) {
        let buffer_size = (self.width * self.height * 4) as usize;

        // Create pixmap
        let mut pixmap = Pixmap::new(self.width, self.height).unwrap();

        // Background color (dark gray, same as candidate window)
        let bg_color = Color::from_rgba8(40, 44, 52, 240);
        pixmap.fill(bg_color);

        // Draw border
        let border_color = Color::from_rgba8(80, 84, 92, 255);
        if let Some(rect) = Rect::from_xywh(0.0, 0.0, self.width as f32, 1.0) {
            let mut paint = Paint::default();
            paint.set_color(border_color);
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }
        if let Some(rect) = Rect::from_xywh(0.0, self.height as f32 - 1.0, self.width as f32, 1.0) {
            let mut paint = Paint::default();
            paint.set_color(border_color);
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }

        // Draw text
        let text_color = Color::from_rgba8(220, 223, 228, 255);
        let padding = 8.0;
        let y = padding + self.renderer.line_height() * 0.75;
        self.renderer
            .draw_text(&mut pixmap, keys, padding, y, text_color);

        // Copy pixmap data to shm buffer
        let dest = &mut self.pool_data[..buffer_size];
        let src = pixmap.data();
        for (i, chunk) in src.chunks(4).enumerate() {
            if i * 4 + 3 < dest.len() {
                // tiny-skia uses premultiplied RGBA, Wayland wants ARGB
                dest[i * 4] = chunk[2]; // B
                dest[i * 4 + 1] = chunk[1]; // G
                dest[i * 4 + 2] = chunk[0]; // R
                dest[i * 4 + 3] = chunk[3]; // A
            }
        }

        // Destroy old buffer if exists
        if let Some(buf) = self.buffer.take() {
            buf.destroy();
        }

        // Create new buffer
        let buffer = self.pool.create_buffer(
            0,
            self.width as i32,
            self.height as i32,
            (self.width * 4) as i32,
            wl_shm::Format::Argb8888,
            qh,
            2usize, // Use 2 as marker for keypress window buffer
        );

        // Attach and commit
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, self.width as i32, self.height as i32);
        self.surface.commit();

        self.buffer = Some(buffer);
    }

    /// Destroy the window
    pub fn destroy(self) {
        if let Some(buf) = self.buffer {
            buf.destroy();
        }
        self.popup_surface.destroy();
        self.surface.destroy();
        self.pool.destroy();
    }
}

/// Create a shared memory pool
fn create_shm_pool(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<State>,
    size: usize,
) -> Option<(wl_shm_pool::WlShmPool, MmapMut)> {
    use std::os::fd::FromRawFd;

    // Create anonymous file with memfd_create
    let fd = unsafe {
        let name = std::ffi::CString::new("ime-keypress-pool").ok()?;
        libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC)
    };

    if fd < 0 {
        eprintln!("[KEYPRESS] Failed to create memfd");
        return None;
    }

    let file = unsafe { std::fs::File::from_raw_fd(fd) };

    // Set file size
    if file.set_len(size as u64).is_err() {
        eprintln!("[KEYPRESS] Failed to set memfd size");
        return None;
    }

    // Memory map the file
    let mmap = unsafe { MmapMut::map_mut(&file) }.ok()?;

    // Create wl_shm_pool
    let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());

    // Keep file alive by leaking it (pool owns the fd now)
    std::mem::forget(file);

    Some((pool, mmap))
}
