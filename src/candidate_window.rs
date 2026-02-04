//! Candidate window using Wayland layer-shell protocol

use memmap2::MmapMut;
use std::os::fd::AsFd;
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::State;
use crate::text_render::{self, TextRenderer};

/// Double buffer state
struct Buffer {
    buffer: wl_buffer::WlBuffer,
    in_use: bool,
}

/// Candidate selection window
pub struct CandidateWindow {
    surface: wl_surface::WlSurface,
    layer_surface: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    pool: wl_shm_pool::WlShmPool,
    pool_data: MmapMut,
    pool_size: usize,
    buffers: [Option<Buffer>; 2],
    current_buffer: usize,
    width: u32,
    height: u32,
    configured: bool,
    pub visible: bool,
    renderer: TextRenderer,
    // Pending render request (candidates, selected) - used when show() called before configure
    pending_render: Option<(Vec<String>, usize)>,
}

impl CandidateWindow {
    /// Create a new candidate window
    pub fn new(
        compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        layer_shell: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<State>,
        renderer: TextRenderer,
    ) -> Option<Self> {
        // Create surface
        let surface = compositor.create_surface(qh, ());

        // Create layer surface (overlay layer, anchored bottom-left)
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None, // Output (None = compositor choice)
            zwlr_layer_shell_v1::Layer::Overlay,
            "ime-candidates".to_string(),
            qh,
            (),
        );

        // Configure layer surface
        layer_surface.set_size(200, 100); // Initial size, will be reconfigured
        layer_surface.set_anchor(
            zwlr_layer_surface_v1::Anchor::Bottom | zwlr_layer_surface_v1::Anchor::Left,
        );
        layer_surface.set_margin(20, 0, 0, 20); // top, right, bottom, left
        layer_surface
            .set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);

        // Commit to get configure event
        surface.commit();

        // Create shm pool for buffers
        // Allocate enough for two 400x400 ARGB buffers (double buffering)
        let pool_size = 400 * 400 * 4 * 2;
        let (pool, pool_data) = create_shm_pool(shm, qh, pool_size)?;

        Some(Self {
            surface,
            layer_surface,
            pool,
            pool_data,
            pool_size,
            buffers: [None, None],
            current_buffer: 0,
            width: 200,
            height: 100,
            configured: false,
            visible: false,
            renderer,
            pending_render: None,
        })
    }

    /// Handle layer surface configure event
    pub fn configure(&mut self, serial: u32, width: u32, height: u32, qh: &QueueHandle<State>) {
        self.layer_surface.ack_configure(serial);

        // Use suggested size or keep our requested size
        if width > 0 {
            self.width = width;
        }
        if height > 0 {
            self.height = height;
        }

        self.configured = true;

        // Process any pending render request
        if let Some((candidates, selected)) = self.pending_render.take() {
            self.show(&candidates, selected, qh);
        }
    }

    /// Show the candidate window with given candidates
    pub fn show(&mut self, candidates: &[String], selected: usize, qh: &QueueHandle<State>) {
        if candidates.is_empty() {
            self.hide();
            return;
        }

        // Calculate required size
        let (new_width, new_height) =
            text_render::calculate_window_size(&mut self.renderer, candidates);

        // If size changed or not configured, we need to wait for a new configure
        let size_changed = new_width != self.width || new_height != self.height;

        if size_changed || !self.configured {
            self.width = new_width;
            self.height = new_height;
            self.layer_surface.set_size(new_width, new_height);
            self.surface.commit();
            self.configured = false;
            self.pending_render = Some((candidates.to_vec(), selected));
            return;
        }

        // Render candidates (size unchanged, already configured)
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
            // After hiding, we need a new configure before showing again
            self.configured = false;
        }
    }

    /// Render candidates to buffer and attach to surface
    fn render(&mut self, candidates: &[String], selected: usize, qh: &QueueHandle<State>) {
        if !self.configured {
            return;
        }

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

        // Render to pixmap
        let pixmap = text_render::render_candidates(
            &mut self.renderer,
            candidates,
            selected,
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
        self.layer_surface.destroy();
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
