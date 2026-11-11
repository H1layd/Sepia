//! Software rasteriser: rectangles, borders, glyphs. No graphics library.

use std::io::BufWriter;
use std::path::Path;

use crate::css::Color;
use crate::fonts::Glyph;
use crate::images::Bitmap;
use crate::style::{Edges, Gradient, Shadow, StopAt};
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
        let x0 = (rect.x.floor() as i32).max(0);
        let y0 = ((rect.y - scroll).floor() as i32).max(0);
        let x1 = ((rect.x.floor() + rect.w.ceil()) as i32).min(self.width as i32);
        let y1 = (((rect.y - scroll).floor() + rect.h.ceil()) as i32).min(self.height as i32);
        for y in y0..y1 {
            self.span(y, x0, x1, color);
        }
    }

    /// Fill pixels x0..x1 of row y at full coverage: a straight copy when
    /// the colour is opaque, a blend otherwise. Coordinates must be clipped.
    fn span(&mut self, y: i32, x0: i32, x1: i32, color: Color) {
        if x1 <= x0 || y < 0 || y as usize >= self.height {
            return;
        }
        let (x0, x1) = (x0.max(0) as usize, (x1 as usize).min(self.width));
        let row = y as usize * self.width;
        if color.a == 255 {
            let px = [color.r, color.g, color.b, 255];
            for chunk in self.pixels[(row + x0) * 4..(row + x1) * 4].chunks_exact_mut(4) {
                chunk.copy_from_slice(&px);
            }
        } else {
            let a = color.a as u32;
            let keep = 255 - a;
            let src = [color.r as u32 * a, color.g as u32 * a, color.b as u32 * a];
            for chunk in self.pixels[(row + x0) * 4..(row + x1) * 4].chunks_exact_mut(4) {
                for c in 0..3 {
                    chunk[c] = ((src[c] + chunk[c] as u32 * keep + 127) / 255) as u8;
                }
                chunk[3] = 255;
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
        self.draw_text_spaced(fonts, text, x, baseline, size, color, bold, italic, scroll, 0.0, None);
    }

    /// `draw_text` with `spacing` device px added after every glyph (letter-spacing).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_spaced(&mut self, fonts: &Fonts, text: &str, x: f32, baseline: f32, size: f32,
                            color: Color, bold: bool, italic: bool, scroll: f32, spacing: f32, family: Option<&str>) {
        let mut pen = x;
        fonts.for_each_glyph_in(family, text, bold, italic, |glyph| {
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
            pen += glyph.advance(size) + spacing;
        });
    }

    /// Visible device rows and columns of `r` (grown by `pad`), clipped to the canvas.
    fn clip(&self, r: &Rect, pad: f32) -> (i32, i32, i32, i32) {
        let x0 = ((r.x - pad).floor() as i32).max(0);
        let x1 = ((r.x + r.w + pad).ceil() as i32).min(self.width as i32);
        let y0 = ((r.y - pad).floor() as i32).max(0);
        let y1 = ((r.y + r.h + pad).ceil() as i32).min(self.height as i32);
        (x0, x1, y0, y1)
    }

    /// A box background: solid colour, then the gradient over it, clipped
    /// to rounded corners. Vertical gradients are evaluated once per row.
    fn fill_box(&mut self, r: Rect, radius: f32, color: Option<Color>, gradient: Option<&Gradient>, scale: f32) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let radius = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        // the gradient as a 1024-step colour table: one lookup per pixel
        let lut = gradient.map(|g| Ramp::new(g, r.w, r.h, scale).table());
        let (x0, x1, y0, y1) = self.clip(&r, 0.0);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            // pixels of this row wholly inside the box take the fast path
            let (ia, ib) = inner_span(&r, radius, y).map(|(a, b)| (a.max(x0), b.min(x1))).unwrap_or((x1, x1));
            let coverage = |x: i32| {
                let px = x as f32 + 0.5;
                if radius > 0.0 { (0.5 - sd_round(px, py, &r, radius)).clamp(0.0, 1.0) } else { edge_coverage(px, py, &r) }
            };
            if let Some(c) = color {
                for x in (x0..ia.min(x1)).chain(ib.max(x0)..x1) {
                    let cov = coverage(x);
                    if cov > 0.0 {
                        self.blend(x, y, c, cov);
                    }
                }
                if ia < ib {
                    self.span(y, ia, ib, c);
                }
            }
            if let Some((table, t0, dtx, dty)) = &lut {
                let row_t = t0 + dty * (py - r.y);
                if dtx.abs() < 1e-7 {
                    // a vertical gradient: one colour for the whole row
                    let c = table[((row_t * 1023.0).round().clamp(0.0, 1023.0)) as usize];
                    for x in (x0..ia.min(x1)).chain(ib.max(x0)..x1) {
                        let cov = coverage(x);
                        if cov > 0.0 {
                            self.blend(x, y, c, cov);
                        }
                    }
                    if ia < ib {
                        self.span(y, ia, ib, c);
                    }
                    continue;
                }
                for x in x0..x1 {
                    let inside = x >= ia && x < ib;
                    let cov = if inside { 1.0 } else { coverage(x) };
                    if cov <= 0.0 {
                        continue;
                    }
                    let t = row_t + dtx * (x as f32 + 0.5 - r.x);
                    let c = table[((t * 1023.0).round().clamp(0.0, 1023.0)) as usize];
                    if inside && c.a == 255 {
                        let i = (y as usize * self.width + x as usize) * 4;
                        self.pixels[i..i + 4].copy_from_slice(&[c.r, c.g, c.b, 255]);
                    } else {
                        self.blend(x, y, c, cov);
                    }
                }
            }
        }
    }

    /// Borders, side by side with their own widths and colours; rounded
    /// corners as one anti-aliased ring.
    fn stroke_box(&mut self, r: Rect, w: &Edges, colors: &[Color; 4], radius: f32) {
        if radius <= 0.5 {
            let sides = [
                (Rect { x: r.x, y: r.y, w: r.w, h: w.top }, colors[0]),
                (Rect { x: r.x, y: r.y + r.h - w.bottom, w: r.w, h: w.bottom }, colors[2]),
                (Rect { x: r.x, y: r.y + w.top, w: w.left, h: r.h - w.top - w.bottom }, colors[3]),
                (Rect { x: r.x + r.w - w.right, y: r.y + w.top, w: w.right, h: r.h - w.top - w.bottom }, colors[1]),
            ];
            for (side, color) in sides {
                if side.w > 0.0 && side.h > 0.0 && color.a > 0 {
                    self.fill_rect(side, color, 0.0);
                }
            }
            return;
        }
        let radius = radius.min(r.w / 2.0).min(r.h / 2.0);
        let inner = Rect { x: r.x + w.left, y: r.y + w.top, w: r.w - w.left - w.right, h: r.h - w.top - w.bottom };
        let inner_radius = (radius - w.top.max(w.left)).max(0.0);
        let (x0, x1, y0, y1) = self.clip(&r, 1.0);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            // the hole's fully covered middle needs no work
            let (ha, hb) = if inner.w > 2.0 && inner.h > 2.0 {
                inner_span(&Rect { x: inner.x + 1.0, y: inner.y + 1.0, w: inner.w - 2.0, h: inner.h - 2.0 }, inner_radius, y).unwrap_or((x1, x1))
            } else {
                (x1, x1)
            };
            for x in (x0..ha.min(x1)).chain(hb.max(x0)..x1) {
                let px = x as f32 + 0.5;
                let outer = (0.5 - sd_round(px, py, &r, radius)).clamp(0.0, 1.0);
                if outer <= 0.0 {
                    continue;
                }
                let hole = if inner.w > 0.0 && inner.h > 0.0 { (0.5 - sd_round(px, py, &inner, inner_radius)).clamp(0.0, 1.0) } else { 0.0 };
                let cov = outer - hole;
                if cov <= 0.0 {
                    continue;
                }
                let color = if py < inner.y {
                    colors[0]
                } else if py > inner.y + inner.h {
                    colors[2]
                } else if px < inner.x {
                    colors[3]
                } else {
                    colors[1]
                };
                self.blend(x, y, color, cov);
            }
        }
    }

    /// An outer box-shadow: the box moved and grown, blurred, and only
    /// where the box itself is not (CSS clips the shadow under the box).
    fn shadow_box(&mut self, r: Rect, radius: f32, shadow: &Shadow, scale: f32) {
        let spread = shadow.spread * scale;
        let blur = (shadow.blur * scale).max(0.5);
        let cast = Rect { x: r.x + shadow.dx * scale - spread, y: r.y + shadow.dy * scale - spread, w: r.w + spread * 2.0, h: r.h + spread * 2.0 };
        let cast_radius = (radius + spread).max(0.0).min(cast.w / 2.0).min(cast.h / 2.0);
        let box_radius = radius.min(r.w / 2.0).min(r.h / 2.0);
        let (x0, x1, y0, y1) = self.clip(&cast, blur);
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for x in x0..x1 {
                let px = x as f32 + 0.5;
                if sd_round(px, py, &r, box_radius) < -0.5 {
                    continue;
                }
                let d = sd_round(px, py, &cast, cast_radius);
                // a smooth step across the blur width, centred on the edge
                let t = (0.5 - d / blur).clamp(0.0, 1.0);
                let a = t * t * (3.0 - 2.0 * t);
                if a > 0.0 {
                    self.blend(x, y, shadow.color, a);
                }
            }
        }
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
        if let Some(background) = page.background {
            if background.a > 0 {
                self.fill_rect(Rect { x: 0.0, y: 0.0, w: self.width as f32, h: self.height as f32 }, background, 0.0);
            }
        }
        let on_screen = |r: &Rect| r.y + r.h >= visible_top && r.y <= visible_bottom;
        for draw in &page.draws {
            match draw {
                Draw::Fill { rect, radius, color, gradient } => {
                    if on_screen(rect) {
                        self.fill_box(map(rect), radius * s, *color, gradient.as_ref(), s);
                    }
                }
                Draw::Edges { rect, widths, colors, radius } => {
                    if on_screen(rect) {
                        let w = Edges { top: widths.top * s, right: widths.right * s, bottom: widths.bottom * s, left: widths.left * s };
                        self.stroke_box(map(rect), &w, colors, radius * s);
                    }
                }
                Draw::Shadow { rect, radius, shadow } => {
                    let pad = shadow.blur + shadow.spread.abs() + shadow.dx.abs() + shadow.dy.abs();
                    let grown = Rect { x: rect.x - pad, y: rect.y - pad, w: rect.w + pad * 2.0, h: rect.h + pad * 2.0 };
                    if on_screen(&grown) {
                        self.shadow_box(map(rect), radius * s, shadow, s);
                    }
                }
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
                    letter_spacing,
                    family,
                } => {
                    if *baseline + size < visible_top || *baseline - size > visible_bottom {
                        continue;
                    }
                    let y = (*baseline - scroll) * s;
                    self.draw_text_spaced(fonts, text, *x * s, y, *size * s, *color, *bold, *italic, 0.0, letter_spacing * s, family.as_deref());
                    if *underline {
                        let width = fonts.advance_in(text, *size, *bold, *italic, family.as_deref()) + letter_spacing * text.chars().count() as f32;
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

/// Pixels [a, b) of row `y` lying wholly inside the rounded rect.
fn inner_span(r: &Rect, radius: f32, y: i32) -> Option<(i32, i32)> {
    let (ya, yb) = (y as f32, y as f32 + 1.0);
    if ya < r.y || yb > r.y + r.h {
        return None;
    }
    let pull = |yy: f32| {
        let dy = if yy < r.y + radius {
            r.y + radius - yy
        } else if yy > r.y + r.h - radius {
            yy - (r.y + r.h - radius)
        } else {
            return 0.0;
        };
        radius - (radius * radius - dy.min(radius).powi(2)).max(0.0).sqrt()
    };
    let dx = pull(ya).max(pull(yb));
    let (a, b) = ((r.x + dx).ceil() as i32, (r.x + r.w - dx).floor() as i32);
    (a < b).then_some((a, b))
}

/// Signed distance from (px, py) to a rounded rectangle (negative inside).
fn sd_round(px: f32, py: f32, r: &Rect, radius: f32) -> f32 {
    let (hw, hh) = (r.w / 2.0, r.h / 2.0);
    let qx = (px - (r.x + hw)).abs() - (hw - radius);
    let qy = (py - (r.y + hh)).abs() - (hh - radius);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius
}

/// Coverage of the pixel centred at (px, py) by an axis-aligned rect: 1
/// inside, fractional on its edges (sub-pixel positions stay smooth).
fn edge_coverage(px: f32, py: f32, r: &Rect) -> f32 {
    let cx = ((px + 0.5).min(r.x + r.w) - (px - 0.5).max(r.x)).clamp(0.0, 1.0);
    let cy = ((py + 0.5).min(r.y + r.h) - (py - 0.5).max(r.y)).clamp(0.0, 1.0);
    cx * cy
}

/// A linear gradient prepared for one box: colour at a point in it.
struct Ramp {
    dir: (f32, f32),
    length: f32,
    center: (f32, f32),
    stops: Vec<(f32, [f32; 4])>,
}

impl Ramp {
    fn new(g: &Gradient, w: f32, h: f32, scale: f32) -> Ramp {
        let a = g.angle.to_radians();
        let dir = (a.sin(), -a.cos());
        let length = (w * dir.0.abs() + h * dir.1.abs()).max(1.0);
        // resolve stop positions: px and fractions, then fill the gaps evenly
        let n = g.stops.len();
        let mut at: Vec<Option<f32>> = g
            .stops
            .iter()
            .map(|(_, p)| match p {
                StopAt::Px(px) => Some(px * scale / length),
                StopAt::Fraction(f) => Some(*f),
                StopAt::Auto => None,
            })
            .collect();
        if n > 0 {
            if at[0].is_none() {
                at[0] = Some(0.0);
            }
            if at[n - 1].is_none() {
                at[n - 1] = Some(1.0);
            }
        }
        let mut i = 0;
        while i < n {
            if at[i].is_none() {
                let start = i - 1;
                let mut end = i;
                while at[end].is_none() {
                    end += 1;
                }
                let (a0, a1) = (at[start].unwrap_or(0.0), at[end].unwrap_or(1.0));
                for k in start + 1..end {
                    at[k] = Some(a0 + (a1 - a0) * (k - start) as f32 / (end - start) as f32);
                }
                i = end;
            }
            i += 1;
        }
        let mut last = f32::MIN;
        let stops = g
            .stops
            .iter()
            .zip(at)
            .map(|((c, _), p)| {
                // positions never go backwards
                let p = p.unwrap_or(0.0).max(last);
                last = p;
                let a = c.a as f32 / 255.0;
                (p, [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, a])
            })
            .collect();
        Ramp { dir, length, center: (w / 2.0, h / 2.0), stops }
    }

    /// A 1024-entry colour table over the gradient line, with the line's
    /// parameter at the box origin and its step per px in x and y.
    fn table(&self) -> (Vec<Color>, f32, f32, f32) {
        let table = (0..1024)
            .map(|i| {
                let t = i as f32 / 1023.0;
                // a point on the gradient line at parameter t
                let d = (t - 0.5) * self.length;
                self.at(self.center.0 + self.dir.0 * d, self.center.1 + self.dir.1 * d)
            })
            .collect();
        let t0 = (-self.center.0 * self.dir.0 - self.center.1 * self.dir.1) / self.length + 0.5;
        (table, t0, self.dir.0 / self.length, self.dir.1 / self.length)
    }

    fn at(&self, x: f32, y: f32) -> Color {
        let t = ((x - self.center.0) * self.dir.0 + (y - self.center.1) * self.dir.1) / self.length + 0.5;
        let stops = &self.stops;
        let mix = if stops.is_empty() {
            [0.0; 4]
        } else if t <= stops[0].0 {
            stops[0].1
        } else if t >= stops[stops.len() - 1].0 {
            stops[stops.len() - 1].1
        } else {
            let k = stops.windows(2).position(|w| t >= w[0].0 && t <= w[1].0).unwrap_or(0);
            let (p0, c0) = stops[k];
            let (p1, c1) = stops[k + 1];
            let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
            [c0[0] + (c1[0] - c0[0]) * f, c0[1] + (c1[1] - c0[1]) * f, c0[2] + (c1[2] - c0[2]) * f, c0[3] + (c1[3] - c0[3]) * f]
        };
        let a = mix[3];
        if a <= 0.0 {
            return Color::rgba(0, 0, 0, 0);
        }
        Color::rgba((mix[0] / a).round() as u8, (mix[1] / a).round() as u8, (mix[2] / a).round() as u8, (a * 255.0).round() as u8)
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
