//! Text rendering for candidate window using fontdb, fontdue, and tiny-skia

use fontdb::{Database, Query};
use fontdue::{Font, FontSettings};
use std::collections::HashMap;
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};

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

/// Render candidate list to a pixmap
pub fn render_candidates(
    renderer: &mut TextRenderer,
    candidates: &[String],
    selected: usize,
    scroll_offset: usize,
    max_visible: usize,
    width: u32,
    height: u32,
) -> Pixmap {
    let mut pixmap = Pixmap::new(width, height).unwrap();

    // Background color (dark gray)
    let bg_color = Color::from_rgba8(40, 44, 52, 255);
    pixmap.fill(bg_color);

    // Colors
    let text_color = Color::from_rgba8(220, 223, 228, 255);
    let selected_bg = Color::from_rgba8(61, 89, 161, 255);
    let number_color = Color::from_rgba8(152, 195, 121, 255);
    let scrollbar_bg = Color::from_rgba8(60, 64, 72, 255);
    let scrollbar_thumb = Color::from_rgba8(100, 104, 112, 255);

    let line_height = renderer.line_height();
    let padding = 8.0;
    let number_width = 24.0;

    let total_count = candidates.len();
    let visible_count = max_visible.min(total_count);
    let has_scrollbar = total_count > max_visible;

    // Render visible candidates
    for (visible_idx, candidate) in candidates
        .iter()
        .skip(scroll_offset)
        .take(visible_count)
        .enumerate()
    {
        let actual_idx = scroll_offset + visible_idx;
        let y_base = padding + (visible_idx as f32 * line_height);
        let y_text = y_base + line_height * 0.75; // Baseline position

        // Draw selection highlight
        if actual_idx == selected {
            let highlight_width = if has_scrollbar {
                width as f32 - SCROLLBAR_WIDTH - 4.0
            } else {
                width as f32
            };
            if let Some(rect) = Rect::from_xywh(0.0, y_base, highlight_width, line_height) {
                let mut paint = Paint::default();
                paint.set_color(selected_bg);
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }

        // Draw number (use actual index + 1)
        let number = format!("{}.", actual_idx + 1);
        renderer.draw_text(&mut pixmap, &number, padding, y_text, number_color);

        // Draw candidate text
        renderer.draw_text(
            &mut pixmap,
            candidate,
            padding + number_width,
            y_text,
            text_color,
        );
    }

    // Draw scrollbar if needed
    if has_scrollbar {
        let scrollbar_x = width as f32 - SCROLLBAR_WIDTH - 2.0;
        let scrollbar_height = height as f32 - padding * 2.0;

        // Scrollbar track
        if let Some(rect) = Rect::from_xywh(scrollbar_x, padding, SCROLLBAR_WIDTH, scrollbar_height)
        {
            let mut paint = Paint::default();
            paint.set_color(scrollbar_bg);
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }

        // Scrollbar thumb
        let thumb_height = (visible_count as f32 / total_count as f32) * scrollbar_height;
        let thumb_height = thumb_height.max(20.0); // Minimum thumb size
        let scroll_range = total_count - visible_count;
        let thumb_y = if scroll_range > 0 {
            padding
                + (scroll_offset as f32 / scroll_range as f32) * (scrollbar_height - thumb_height)
        } else {
            padding
        };

        if let Some(rect) = Rect::from_xywh(scrollbar_x, thumb_y, SCROLLBAR_WIDTH, thumb_height) {
            let mut paint = Paint::default();
            paint.set_color(scrollbar_thumb);
            pixmap.fill_rect(rect, &paint, Transform::identity(), None);
        }
    }

    pixmap
}

/// Scrollbar width in pixels
const SCROLLBAR_WIDTH: f32 = 8.0;

/// Calculate required window size for candidates
pub fn calculate_window_size(
    renderer: &mut TextRenderer,
    candidates: &[String],
    has_scrollbar: bool,
) -> (u32, u32) {
    let line_height = renderer.line_height();
    let padding = 8.0;
    let number_width = 24.0;
    let scrollbar_space = if has_scrollbar {
        SCROLLBAR_WIDTH + 4.0
    } else {
        0.0
    };

    // Calculate max width needed
    let mut max_width = 200.0f32; // Minimum width
    for candidate in candidates {
        let text_width = renderer.measure_text(candidate);
        max_width = max_width.max(text_width + number_width + padding * 2.0 + scrollbar_space);
    }

    let height = (candidates.len() as f32 * line_height + padding * 2.0) as u32;
    let width = max_width.ceil() as u32;

    // Align to 4 bytes for wl_shm
    let width = (width + 3) & !3;

    (width.max(100), height.max(30))
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

    eprintln!("[FONT] Loaded {} font faces", db.faces().count());

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
            eprintln!("[FONT] Found font: {} ({})", family, face.post_script_name);

            // Load the font data
            if let Some(font_data) = db.face_source(id) {
                match &font_data.0 {
                    fontdb::Source::File(path) => {
                        if let Ok(data) = std::fs::read(path)
                            && let Ok(font) = Font::from_bytes(data, FontSettings::default())
                        {
                            eprintln!("[FONT] Loaded: {}", path.display());
                            return Some(font);
                        }
                    }
                    fontdb::Source::Binary(data) => {
                        let bytes: Vec<u8> = data.as_ref().as_ref().to_vec();
                        if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                            eprintln!("[FONT] Loaded from memory");
                            return Some(font);
                        }
                    }
                    fontdb::Source::SharedFile(_, data) => {
                        let bytes: Vec<u8> = data.as_ref().as_ref().to_vec();
                        if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                            eprintln!("[FONT] Loaded from memory");
                            return Some(font);
                        }
                    }
                }
            }
        }
    }

    eprintln!("[FONT] Warning: No Japanese font found, candidate window disabled");
    None
}
