//! Software rasteriser: rectangles, borders, glyphs. No graphics library.

use std::io::BufWriter;
use std::path::Path;

use crate::css::Color;
use crate::fonts::Glyph;
use crate::images::Bitmap;
use crate::layout::{Draw, Fonts, Page, Rect};

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Canvas {
    pub fn new(width: usize, height: usize, background: Color) -> Canvas {
        let mut pixels = vec![0u8; width * height * 4];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[0] = background.r;
            chunk[1] = background.g;
            chunk[2] = background.b;
            chunk[3] = 255;
        }
        Canvas { width, height, pixels }
    }

    fn blend(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let alpha = (coverage * color.a as f32 / 255.0).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let index = (y as usize * self.width + x as usize) * 4;
        let src = [color.r as f32, color.g as f32, color.b as f32];
        for channel in 0..3 {
            let dst = self.pixels[index + channel] as f32;
            self.pixels[index + channel] = (src[channel] * alpha + dst * (1.0 - alpha)).round() as u8;
        }
        self.pixels[index + 3] = 255;
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color, scroll: f32) {
        if color.a == 0 {
            return;
        }
        let x0 = rect.x.floor() as i32;
        let y0 = (rect.y - scroll).floor() as i32;
        let w = rect.w.ceil() as i32;
        let h = rect.h.ceil() as i32;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.blend(x, y, color, 1.0);
            }
        }
    }

    pub fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color, scroll: f32) {
        let width = width.max(1.0);
        let top = Rect { x: rect.x, y: rect.y, w: rect.w, h: width };
        let bottom = Rect { x: rect.x, y: rect.y + rect.h - width, w: rect.w, h: width };
        let left = Rect { x: rect.x, y: rect.y, w: width, h: rect.h };
        let right = Rect { x: rect.x + rect.w - width, y: rect.y, w: width, h: rect.h };
        for piece in [top, bottom, left, right] {
            self.fill_rect(piece, color, scroll);
        }
    }

    pub fn draw_text(&mut self, fonts: &Fonts, text: &str, x: f32, baseline: f32, size: f32,
                     color: Color, bold: bool, italic: bool, scroll: f32) {
        let mut pen = x;
        fonts.for_each_glyph(text, bold, italic, |glyph| {
            match &glyph {
                Glyph::Outline(font, ch) => {
                    let (metrics, coverage) = font.rasterize(*ch, size);
                    let glyph_x = pen + metrics.xmin as f32;
                    // Glyphs are positioned relative to the baseline.
                    let glyph_y = baseline - (metrics.ymin as f32 + metrics.height as f32) - scroll;
                    if metrics.width > 0 && metrics.height > 0 {
                        for gy in 0..metrics.height {
                            for gx in 0..metrics.width {
                                let value = coverage[gy * metrics.width + gx] as f32 / 255.0;
                                if value > 0.0 {
                                    self.blend(
                                        glyph_x.round() as i32 + gx as i32,
                                        glyph_y.round() as i32 + gy as i32,
                                        color,
                                        value,
                                    );
                                }
                            }
                        }
                    }
                }
                Glyph::Emoji(face, gid) => {
                    if let Some(bitmap) = face.bitmap(*gid, size) {
                        let left = (pen + bitmap.left).round() as i32;
                        let top = (baseline + bitmap.top - scroll).round() as i32;
                        self.blit_premultiplied(&bitmap.rgba, bitmap.width, bitmap.height, left, top);
                    }
                }
            }
            pen += glyph.advance(size);
        });
    }

    /// An image scaled into `rect` (device px), or, when it could not be
    /// loaded, a light frame with the alt text clipped to the box.
    fn draw_image(&mut self, fonts: &Fonts, rect: Rect, image: Option<&Bitmap>, alt: &str, scale: f32) {
        let (w, h) = (rect.w.round() as usize, rect.h.round() as usize);
        if w == 0 || h == 0 {
            return;
        }
        let (left, top) = (rect.x.round() as i32, rect.y.round() as i32);
        match image {
            Some(bitmap) => {
                let pixels = bitmap.scaled(w, h);
                self.blit_premultiplied(&pixels, w, h, left, top);
            }
            None => {
                let frame = Color { r: 200, g: 200, b: 200, a: 255 };
                self.fill_rect(rect, Color { r: 246, g: 246, b: 246, a: 255 }, 0.0);
                self.stroke_rect(rect, scale.max(1.0), frame, 0.0);
                let size = 12.0 * scale;
                if !alt.is_empty() && rect.h >= size * 1.6 {
                    let room = rect.w - 8.0 * scale;
                    let mut shown = String::new();
                    for ch in alt.chars() {
                        shown.push(ch);
                        if fonts.advance(&shown, size, false, false) > room {
                            shown.pop();
                            break;
                        }
                    }
                    let gray = Color { r: 110, g: 110, b: 110, a: 255 };
                    self.draw_text(fonts, &shown, rect.x + 4.0 * scale, rect.y + size * 1.25, size, gray, false, false, 0.0);
                }
            }
        }
    }

    /// Composite a premultiplied RGBA bitmap (colour emoji): dst = src + dst·(1−a).
    fn blit_premultiplied(&mut self, rgba: &[u8], w: usize, h: usize, left: i32, top: i32) {
        for gy in 0..h {
            let y = top + gy as i32;
            if y < 0 || y as usize >= self.height {
                continue;
            }
            for gx in 0..w {
                let x = left + gx as i32;
                if x < 0 || x as usize >= self.width {
                    continue;
                }
                let s = &rgba[(gy * w + gx) * 4..(gy * w + gx) * 4 + 4];
                if s[3] == 0 {
                    continue;
                }
                let index = (y as usize * self.width + x as usize) * 4;
                let keep = 255 - s[3] as u32;
                for c in 0..3 {
                    let dst = self.pixels[index + c] as u32;
                    self.pixels[index + c] = (s[c] as u32 + (dst * keep + 127) / 255).min(255) as u8;
                }
                self.pixels[index + 3] = 255;
            }
        }
    }

    pub fn paint(&mut self, fonts: &Fonts, page: &Page, scroll: f32) {
        self.paint_scaled(fonts, page, scroll, 1.0);
    }

    /// Paint at `scale` device pixels per CSS px (2.0 on Retina): layout stays
    /// in CSS px, glyphs are rasterised at the device size so text stays sharp.
    /// Draws entirely outside the canvas are skipped.
    pub fn paint_scaled(&mut self, fonts: &Fonts, page: &Page, scroll: f32, scale: f32) {
        let s = scale;
        let visible_top = scroll - 64.0;
        let visible_bottom = scroll + self.height as f32 / s + 64.0;
        let map = |r: &Rect| Rect { x: r.x * s, y: (r.y - scroll) * s, w: r.w * s, h: r.h * s };
        for draw in &page.draws {
            match draw {
                Draw::Rect { rect, color } => {
                    if rect.y + rect.h >= visible_top && rect.y <= visible_bottom {
                        self.fill_rect(map(rect), *color, 0.0)
                    }
                }
                Draw::Border { rect, width, color } => {
                    if rect.y + rect.h >= visible_top && rect.y <= visible_bottom {
                        self.stroke_rect(map(rect), *width * s, *color, 0.0)
                    }
                }
                Draw::Image { rect, image, alt } => {
                    if rect.y + rect.h < visible_top || rect.y > visible_bottom {
                        continue;
                    }
                    self.draw_image(fonts, map(rect), image.as_deref(), alt, s);
                }
                Draw::Text {
                    x,
                    baseline,
                    text,
                    size,
                    color,
                    bold,
                    italic,
                    underline,
                } => {
                    if *baseline + size < visible_top || *baseline - size > visible_bottom {
                        continue;
                    }
                    let y = (*baseline - scroll) * s;
                    self.draw_text(fonts, text, *x * s, y, *size * s, *color, *bold, *italic, 0.0);
                    if *underline {
                        let width = fonts.advance(text, *size, *bold, *italic);
                        self.fill_rect(
                            Rect { x: *x * s, y: y + 1.5 * s, w: width * s, h: s.max(1.0) },
                            *color,
                            0.0,
                        );
                    }
                }
            }
        }
    }

    pub fn save_png(&self, path: &Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        let writer = BufWriter::new(file);
        let mut encoder = png::Encoder::new(writer, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut png_writer = encoder
            .write_header()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        png_writer
            .write_image_data(&self.pixels)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_clips() {
        let mut canvas = Canvas::new(10, 10, Color::WHITE);
        canvas.fill_rect(Rect { x: -5.0, y: -5.0, w: 8.0, h: 8.0 }, Color::BLACK, 0.0);
        assert_eq!(&canvas.pixels[0..4], &[0, 0, 0, 255]);
        assert_eq!(&canvas.pixels[(9 * 10 + 9) * 4..(9 * 10 + 9) * 4 + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn text_marks_pixels() {
        let fonts = Fonts::load();
        let mut canvas = Canvas::new(120, 40, Color::WHITE);
        canvas.draw_text(&fonts, "Hi", 4.0, 26.0, 20.0, Color::BLACK, false, false, 0.0);
        let dark = canvas.pixels.chunks_exact(4).filter(|p| p[0] < 128).count();
        assert!(dark > 5, "expected glyph pixels, got {dark}");
    }
}
