//! Text rendering for candidate window using fontdb, fontdue, and tiny-skia

use fontdb::{Database, Query};
use fontdue::{Font, FontSettings};
use memmap2::MmapMut;
use std::collections::HashMap;
use std::os::fd::AsFd;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_shm, wl_shm_pool};

use crate::State;

/// Font renderer with glyph caching
pub struct TextRenderer {
    font: Font,
    font_size: f32,
    glyph_cache: HashMap<char, GlyphData>,
}

#[derive(Clone)]
struct GlyphData {
    metrics: fontdue::Metrics,
    bitmap: Vec<u8>,
}

impl TextRenderer {
    /// Create a new text renderer, searching for a Japanese-capable font
    pub fn new(font_size: f32) -> Option<Self> {
        let font = load_japanese_font()?;
        Some(Self {
            font,
            font_size,
            glyph_cache: HashMap::new(),
        })
    }

    /// Get or rasterize a glyph (returns owned data to avoid borrow issues)
    fn get_glyph(&mut self, c: char) -> GlyphData {
        if !self.glyph_cache.contains_key(&c) {
            let (metrics, bitmap) = self.font.rasterize(c, self.font_size);
            self.glyph_cache.insert(c, GlyphData { metrics, bitmap });
        }
        self.glyph_cache.get(&c).unwrap().clone()
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

/// Load a Japanese-capable font from system fonts
fn load_japanese_font() -> Option<Font> {
    let mut db = Database::new();

    // Load system fonts
    db.load_system_fonts();

    // Also check common paths
    let common_paths = ["/usr/share/fonts", "/usr/local/share/fonts"];
    for path in common_paths {
        if std::path::Path::new(path).exists() {
            db.load_fonts_dir(path);
        }
    }

    // Load user fonts
    if let Some(home) = std::env::var_os("HOME") {
        let user_fonts = std::path::PathBuf::from(home).join(".local/share/fonts");
        if user_fonts.exists() {
            db.load_fonts_dir(user_fonts);
        }
    }

    log::info!("[FONT] Loaded {} font faces", db.faces().count());

    // Preferred fonts for Japanese text
    let preferred_families = [
        "Noto Sans CJK JP",
        "Noto Sans CJK",
        "Source Han Sans",
        "Source Han Sans JP",
        "M+ 1p",
        "IPAGothic",
        "IPAPGothic",
        "VL Gothic",
        "TakaoGothic", // Note: no space
        "TakaoPGothic",
        "Noto Sans Mono", // Fallback (limited Japanese but widely available)
        "Liberation Sans",
        "DejaVu Sans",
    ];

    for family in preferred_families {
        let query = Query {
            families: &[fontdb::Family::Name(family)],
            ..Query::default()
        };

        if let Some(id) = db.query(&query)
            && let Some(face) = db.face(id)
        {
            log::debug!("[FONT] Found font: {} ({})", family, face.post_script_name);

            // Load the font data
            if let Some(font_data) = db.face_source(id) {
                match &font_data.0 {
                    fontdb::Source::File(path) => {
                        if let Ok(data) = std::fs::read(path)
                            && let Ok(font) = Font::from_bytes(data, FontSettings::default())
                        {
                            log::debug!("[FONT] Loaded: {}", path.display());
                            return Some(font);
                        }
                    }
                    fontdb::Source::Binary(data) => {
                        let bytes: Vec<u8> = data.as_ref().as_ref().to_vec();
                        if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                            log::debug!("[FONT] Loaded from memory");
                            return Some(font);
                        }
                    }
                    fontdb::Source::SharedFile(_, data) => {
                        let bytes: Vec<u8> = data.as_ref().as_ref().to_vec();
                        if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                            log::debug!("[FONT] Loaded from memory");
                            return Some(font);
                        }
                    }
                }
            }
        }
    }

    log::warn!("[FONT] No Japanese font found, candidate window disabled");
    None
}
