//! Candidate window using Wayland input-method popup surface protocol
//!
//! Uses zwp_input_popup_surface_v2 which is automatically positioned near
//! the text cursor by the compositor.

use memmap2::MmapMut;
use std::os::fd::AsFd;
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_v2, zwp_input_popup_surface_v2,
};

use super::text_render::{self, TextRenderer};
use crate::State;

/// Double buffer state
struct Buffer {
    buffer: wl_buffer::WlBuffer,
    in_use: bool,
}

/// Maximum number of visible candidates (scrollable)
const MAX_VISIBLE_CANDIDATES: usize = 9;

/// Candidate selection window
pub struct CandidateWindow {
    surface: wl_surface::WlSurface,
    popup_surface: zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
    pool: wl_shm_pool::WlShmPool,
    pool_data: MmapMut,
    pool_size: usize,
    buffers: [Option<Buffer>; 2],
    current_buffer: usize,
    width: u32,
    height: u32,
    pub visible: bool,
    renderer: TextRenderer,
    // Scroll offset (index of first visible candidate)
    scroll_offset: usize,
}

impl CandidateWindow {
    /// Create a new candidate window using input method popup surface
    ///
    /// The popup surface is automatically positioned near the text cursor
    /// by the compositor.
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

        // Create shm pool for buffers
        // Allocate enough for two 400x400 ARGB buffers (double buffering)
        let pool_size = 400 * 400 * 4 * 2;
        let (pool, pool_data) = create_shm_pool(shm, qh, pool_size)?;

        Some(Self {
            surface,
            popup_surface,
            pool,
            pool_data,
            pool_size,
            buffers: [None, None],
            current_buffer: 0,
            width: 200,
            height: 100,
            visible: false,
            renderer,
            scroll_offset: 0,
        })
    }

    /// Show the candidate window with given candidates
    ///
    /// The popup surface is automatically shown by the compositor when
    /// the input method is active, so we just need to render the content.
    pub fn show(&mut self, candidates: &[String], selected: usize, qh: &QueueHandle<State>) {
        if candidates.is_empty() {
            self.hide();
            return;
        }

        // Adjust scroll offset to keep selection visible
        let visible_count = MAX_VISIBLE_CANDIDATES.min(candidates.len());
        if selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if selected >= self.scroll_offset + visible_count {
            self.scroll_offset = selected - visible_count + 1;
        }

        // Calculate required size (based on visible candidates, not all)
        let visible_candidates: Vec<_> = candidates
            .iter()
            .skip(self.scroll_offset)
            .take(visible_count)
            .cloned()
            .collect();
        let has_scrollbar = candidates.len() > MAX_VISIBLE_CANDIDATES;
        let (new_width, new_height) = text_render::calculate_window_size(
            &mut self.renderer,
            &visible_candidates,
            has_scrollbar,
        );

        self.width = new_width;
        self.height = new_height;

        // Render candidates
        self.render(candidates, selected, qh);
        self.visible = true;
    }

    /// Hide the candidate window
    pub fn hide(&mut self) {
        if self.visible {
            // Attach null buffer to hide
            self.surface.attach(None, 0, 0);
            self.surface.commit();
            self.visible = false;
            // Reset scroll position
            self.scroll_offset = 0;
        }
    }

    /// Render candidates to buffer and attach to surface
    fn render(&mut self, candidates: &[String], selected: usize, qh: &QueueHandle<State>) {
        // Ensure pool is large enough
        let buffer_size = (self.width * self.height * 4) as usize;
        if buffer_size * 2 > self.pool_size {
            eprintln!(
                "[CANDIDATE] Warning: buffer too large ({}x{}), skipping render",
                self.width, self.height
            );
            return;
        }

        // Find available buffer slot
        let buffer_idx = self.find_available_buffer();
        let offset = buffer_idx * buffer_size;

        // Render to pixmap with scroll info
        let pixmap = text_render::render_candidates(
            &mut self.renderer,
            candidates,
            selected,
            self.scroll_offset,
            MAX_VISIBLE_CANDIDATES,
            self.width,
            self.height,
        );

        // Copy pixmap data to shm buffer
        let dest = &mut self.pool_data[offset..offset + buffer_size];

        // Convert RGBA to ARGB (Wayland expects ARGB8888)
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

        // Get or create wl_buffer for this slot
        if self.buffers[buffer_idx].is_none() {
            let buffer = self.pool.create_buffer(
                offset as i32,
                self.width as i32,
                self.height as i32,
                (self.width * 4) as i32, // stride
                wl_shm::Format::Argb8888,
                qh,
                buffer_idx, // Use buffer index as user data
            );
            self.buffers[buffer_idx] = Some(Buffer {
                buffer,
                in_use: true,
            });
        } else {
            // Recreate buffer if dimensions changed (destroy old, create new)
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

    /// Find an available buffer slot (not currently in use by compositor)
    fn find_available_buffer(&mut self) -> usize {
        // Prefer the non-current buffer
        let other = 1 - self.current_buffer;
        if self.buffers[other]
            .as_ref()
            .map(|b| !b.in_use)
            .unwrap_or(true)
        {
            return other;
        }

        // Fall back to current buffer
        self.current_buffer
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
        let name = std::ffi::CString::new("ime-candidate-pool").ok()?;
        libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC)
    };

    if fd < 0 {
        eprintln!("[CANDIDATE] Failed to create memfd");
        return None;
    }

    let file = unsafe { std::fs::File::from_raw_fd(fd) };

    // Set file size
    if file.set_len(size as u64).is_err() {
        eprintln!("[CANDIDATE] Failed to set memfd size");
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
