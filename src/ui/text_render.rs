//! Text rendering for candidate window using fontconfig, fontdue, and tiny-skia

use fontconfig::{FC_CHARSET, Fontconfig};
use fontconfig_sys as sys;
use fontconfig_sys::ffi_dispatch;
// Without dlopen, ffi_dispatch! expands to direct function calls from sys::*
use fontdue::{Font, FontSettings};
use memmap2::MmapMut;
use std::collections::HashMap;
use std::os::fd::AsFd;
use std::sync::Arc;
use sys::*;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_shm, wl_shm_pool};

use crate::State;

/// Font renderer with glyph caching and per-glyph font fallback
pub struct TextRenderer {
    font: Font,
    fallback_fonts: Vec<Font>,
    fc: Fontconfig,
    font_size: f32,
    glyph_cache: HashMap<char, GlyphData>,
}

#[derive(Clone)]
struct GlyphData {
    metrics: fontdue::Metrics,
    bitmap: Arc<[u8]>,
}

impl TextRenderer {
    /// Create a new text renderer, searching for a font via fontconfig
    pub fn new(font_size: f32) -> Option<Self> {
        let (font, fc) = load_font()?;
        Some(Self {
            font,
            fallback_fonts: Vec::new(),
            fc,
            font_size,
            glyph_cache: HashMap::new(),
        })
    }

    /// Create a text renderer preferring monospace fonts.
    /// Falls back to the default font if fontconfig has no monospace match.
    pub fn new_monospace(font_size: f32) -> Option<Self> {
        if let Some((font, fc)) = load_font_with_family(Some("monospace")) {
            Some(Self {
                font,
                fallback_fonts: Vec::new(),
                fc,
                font_size,
                glyph_cache: HashMap::new(),
            })
        } else {
            Self::new(font_size)
        }
    }

    /// Get or rasterize a glyph with font fallback
    fn get_glyph(&mut self, c: char) -> GlyphData {
        if let Some(cached) = self.glyph_cache.get(&c) {
            return cached.clone();
        }

        // Try primary font
        if self.font.has_glyph(c) {
            let (metrics, bitmap) = self.font.rasterize(c, self.font_size);
            let data = GlyphData {
                metrics,
                bitmap: bitmap.into(),
            };
            self.glyph_cache.insert(c, data.clone());
            return data;
        }

        // Try existing fallback fonts
        for fb in &self.fallback_fonts {
            if fb.has_glyph(c) {
                let (metrics, bitmap) = fb.rasterize(c, self.font_size);
                let data = GlyphData {
                    metrics,
                    bitmap: bitmap.into(),
                };
                self.glyph_cache.insert(c, data.clone());
                return data;
            }
        }

        // Query fontconfig for a fallback font covering this character
        if let Some(fb) = self.query_fallback_font(c) {
            let (metrics, bitmap) = fb.rasterize(c, self.font_size);
            let data = GlyphData {
                metrics,
                bitmap: bitmap.into(),
            };
            self.glyph_cache.insert(c, data.clone());
            self.fallback_fonts.push(fb);
            return data;
        }

        // Last resort: primary font's .notdef glyph
        let (metrics, bitmap) = self.font.rasterize(c, self.font_size);
        let data = GlyphData {
            metrics,
            bitmap: bitmap.into(),
        };
        self.glyph_cache.insert(c, data.clone());
        data
    }

    /// Query fontconfig for a font that covers the given character
    #[allow(unexpected_cfgs)] // ffi_dispatch! macro checks cfg(feature = "dlopen") internally
    fn query_fallback_font(&self, c: char) -> Option<Font> {
        unsafe {
            let cs = ffi_dispatch!(LIB, FcCharSetCreate,);
            ffi_dispatch!(LIB, FcCharSetAddChar, cs, c as u32);

            let mut pat = fontconfig::Pattern::new(&self.fc);
            ffi_dispatch!(
                LIB,
                FcPatternAddCharSet,
                pat.as_mut_ptr(),
                FC_CHARSET.as_ptr(),
                cs
            );
            let matched = pat.font_match();
            ffi_dispatch!(LIB, FcCharSetDestroy, cs);

            let path = matched.filename()?;
            let index = matched.face_index().unwrap_or(0) as u32;

            let data = std::fs::read(path)
                .map_err(|e| log::warn!("[FONT] Failed to read fallback {}: {}", path, e))
                .ok()?;

            let font = Font::from_bytes(
                data,
                FontSettings {
                    collection_index: index,
                    ..Default::default()
                },
            )
            .map_err(|e| log::warn!("[FONT] Failed to parse fallback {}: {}", path, e))
            .ok()?;

            log::info!("[FONT] Fallback for '{}': {} (index={})", c, path, index);
            Some(font)
        }
    }

    /// Measure text width
    pub fn measure_text(&mut self, text: &str) -> f32 {
        let mut width = 0.0;
        for c in text.chars() {
            let glyph = self.get_glyph(c);
            width += glyph.metrics.advance_width;
        }
        width
    }

    /// Get line height (includes some padding)
    pub fn line_height(&self) -> f32 {
        self.font_size * 1.4
    }

    /// Draw text at position
    pub fn draw_text(&mut self, pixmap: &mut Pixmap, text: &str, x: f32, y: f32, color: Color) {
        let mut cursor_x = x;

        for c in text.chars() {
            let glyph = self.get_glyph(c);

            // Calculate glyph position
            let glyph_x = cursor_x + glyph.metrics.xmin as f32;
            let glyph_y = y - glyph.metrics.ymin as f32 - glyph.metrics.height as f32;

            // Draw glyph bitmap
            if glyph.metrics.width > 0 && glyph.metrics.height > 0 {
                draw_glyph_bitmap(
                    pixmap,
                    &glyph.bitmap,
                    glyph.metrics.width,
                    glyph.metrics.height,
                    glyph_x as i32,
                    glyph_y as i32,
                    color,
                );
            }

            cursor_x += glyph.metrics.advance_width;
        }
    }
}

fn draw_glyph_bitmap(
    pixmap: &mut Pixmap,
    bitmap: &[u8],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    color: Color,
) {
    let pixmap_width = pixmap.width() as i32;
    let pixmap_height = pixmap.height() as i32;
    let pixels = pixmap.pixels_mut();

    for gy in 0..height {
        for gx in 0..width {
            let px = x + gx as i32;
            let py = y + gy as i32;

            if px >= 0 && px < pixmap_width && py >= 0 && py < pixmap_height {
                let alpha = bitmap[gy * width + gx];
                if alpha > 0 {
                    let idx = (py * pixmap_width + px) as usize;
                    let existing = pixels[idx];

                    // Alpha blend
                    let a = (alpha as f32 / 255.0) * color.alpha();
                    let inv_a = 1.0 - a;

                    let r = (color.red() * a + existing.red() as f32 / 255.0 * inv_a) * 255.0;
                    let g = (color.green() * a + existing.green() as f32 / 255.0 * inv_a) * 255.0;
                    let b = (color.blue() * a + existing.blue() as f32 / 255.0 * inv_a) * 255.0;

                    pixels[idx] =
                        tiny_skia::PremultipliedColorU8::from_rgba(r as u8, g as u8, b as u8, 255)
                            .unwrap();
                }
            }
        }
    }
}

/// Create a shared memory pool for Wayland surfaces
pub fn create_shm_pool(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<State>,
    size: usize,
    name: &str,
) -> Option<(wl_shm_pool::WlShmPool, MmapMut)> {
    use std::os::fd::FromRawFd;

    // Create anonymous file with memfd_create
    let fd = unsafe {
        let c_name = std::ffi::CString::new(name).ok()?;
        libc::memfd_create(c_name.as_ptr(), libc::MFD_CLOEXEC)
    };

    if fd < 0 {
        log::error!("[SHM] Failed to create memfd for {}", name);
        return None;
    }

    let file = unsafe { std::fs::File::from_raw_fd(fd) };

    // Set file size
    if file.set_len(size as u64).is_err() {
        log::error!("[SHM] Failed to set memfd size for {}", name);
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

/// Copy pixmap data to SHM buffer, converting RGBA to ARGB (Wayland format)
pub fn copy_pixmap_to_shm(pixmap: &Pixmap, dest: &mut [u8]) {
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
}

/// Draw a 1-pixel border around the pixmap
pub fn draw_border(pixmap: &mut Pixmap, width: u32, height: u32, color: Color) {
    let mut paint = Paint::default();
    paint.set_color(color);

    // Top
    if let Some(rect) = Rect::from_xywh(0.0, 0.0, width as f32, 1.0) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
    // Bottom
    if let Some(rect) = Rect::from_xywh(0.0, height as f32 - 1.0, width as f32, 1.0) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
    // Left
    if let Some(rect) = Rect::from_xywh(0.0, 0.0, 1.0, height as f32) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
    // Right
    if let Some(rect) = Rect::from_xywh(width as f32 - 1.0, 0.0, 1.0, height as f32) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

/// Find and load a font via fontconfig (automatic detection, no preferences).
fn load_font() -> Option<(Font, Fontconfig)> {
    load_font_with_family(None)
}

/// Load a font via fontconfig, optionally requesting a specific family (e.g., "monospace").
#[allow(unexpected_cfgs)]
fn load_font_with_family(family: Option<&str>) -> Option<(Font, Fontconfig)> {
    let fc = Fontconfig::new().or_else(|| {
        log::warn!("[FONT] Failed to initialize fontconfig");
        None
    })?;

    // Extract path and index from fontconfig match, then drop patterns to release borrow on fc
    let (path, index) = {
        let mut pat = fontconfig::Pattern::new(&fc);
        if let Some(family_name) = family {
            unsafe {
                let c_family = std::ffi::CString::new("family").ok()?;
                let c_value = std::ffi::CString::new(family_name).ok()?;
                ffi_dispatch!(
                    LIB,
                    FcPatternAddString,
                    pat.as_mut_ptr(),
                    c_family.as_ptr(),
                    c_value.as_ptr() as *const u8
                );
            }
        }
        let matched = pat.font_match();

        let path = matched.filename().or_else(|| {
            log::warn!("[FONT] fontconfig returned no filename");
            None
        })?;
        let index = matched.face_index().unwrap_or(0) as u32;
        (path.to_owned(), index)
    };

    let data = std::fs::read(&path)
        .map_err(|e| {
            log::warn!("[FONT] Failed to read {}: {}", path, e);
        })
        .ok()?;

    let font = Font::from_bytes(
        data,
        FontSettings {
            collection_index: index,
            ..Default::default()
        },
    )
    .map_err(|e| {
        log::warn!("[FONT] Failed to parse {}: {}", path, e);
    })
    .ok()?;

    let family_label = family.unwrap_or("default");
    log::info!(
        "[FONT] Loaded ({}): {} (index={})",
        family_label,
        path,
        index
    );
    Some((font, fc))
}
