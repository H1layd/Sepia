//! `<canvas>` 2D drawing: the bitmaps behind `getContext('2d')`.
//!
//! The context object lives in the prelude (js_prelude.js): it keeps the
//! drawing state (styles, transform, path), turns paths into device
//! coordinates as the spec says (points are transformed when added), and
//! calls the natives here, which rasterize with tiny-skia. Each canvas is a
//! premultiplied RGBA pixmap keyed by its DOM node id; layout shows it as a
//! replaced element under the image key `sepia-canvas:<id>`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tiny_skia::{
    BlendMode, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder, Pattern,
    Pixmap, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

use crate::fonts::Glyph;
use crate::layout::Fonts;
use crate::images::Bitmap;

struct Surface {
    pixmap: Pixmap,
    clip: Option<Mask>,
}

thread_local! {
    static SURFACES: RefCell<HashMap<usize, Surface>> = RefCell::new(HashMap::new());
    static DIRTY: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// The image key layout looks a canvas up by.
pub fn key(id: usize) -> String {
    format!("sepia-canvas:{id}")
}

fn with_surface<R>(id: usize, f: impl FnOnce(&mut Surface) -> R) -> Option<R> {
    let out = SURFACES.with(|s| s.borrow_mut().get_mut(&id).map(f));
    if out.is_some() {
        DIRTY.with(|d| d.borrow_mut().insert(id));
    }
    out
}

/// (Re)size a canvas: a new, transparent bitmap (what setting width/height does).
pub fn init(id: usize, w: u32, h: u32) {
    let pixmap = Pixmap::new(w.clamp(1, 16384), h.clamp(1, 16384)).expect("size is clamped");
    SURFACES.with(|s| s.borrow_mut().insert(id, Surface { pixmap, clip: None }));
    DIRTY.with(|d| d.borrow_mut().insert(id));
}

/// Canvases drawn on since the last call, as bitmaps for layout.
pub fn take_dirty() -> Vec<(String, Arc<Bitmap>)> {
    let ids: Vec<usize> = DIRTY.with(|d| d.borrow_mut().drain().collect());
    SURFACES.with(|s| {
        let s = s.borrow();
        ids.into_iter()
            .filter_map(|id| s.get(&id).map(|surf| (key(id), Arc::new(to_bitmap(&surf.pixmap)))))
            .collect()
    })
}

fn to_bitmap(p: &Pixmap) -> Bitmap {
    Bitmap::from_premultiplied(p.width() as usize, p.height() as usize, p.data().to_vec())
}

/// A canvas as a pixmap (for drawImage of one canvas onto another).
fn snapshot(id: usize) -> Option<Pixmap> {
    SURFACES.with(|s| s.borrow().get(&id).map(|surf| surf.pixmap.clone()))
}

// ----- parsing what the prelude sends ---------------------------------------

fn numbers(text: &str) -> Vec<f32> {
    text.split(',').filter_map(|n| n.trim().parse::<f32>().ok()).collect()
}

pub fn transform(text: &str) -> Transform {
    match numbers(text).as_slice() {
        [a, b, c, d, e, f] => Transform::from_row(*a, *b, *c, *d, *e, *f),
        _ => Transform::identity(),
    }
}

/// Path commands in device space: `M x y`, `L x y`, `Q cx cy x y`,
/// `C c1x c1y c2x c2y x y`, `Z`, space separated.
pub fn path(text: &str) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let mut it = text.split_ascii_whitespace();
    let num = |it: &mut std::str::SplitAsciiWhitespace| it.next().and_then(|n| n.parse::<f32>().ok()).filter(|n| n.is_finite());
    while let Some(op) = it.next() {
        match op {
            "M" => { let (x, y) = (num(&mut it)?, num(&mut it)?); pb.move_to(x, y); }
            "L" => { let (x, y) = (num(&mut it)?, num(&mut it)?); pb.line_to(x, y); }
            "Q" => { let v = [num(&mut it)?, num(&mut it)?, num(&mut it)?, num(&mut it)?]; pb.quad_to(v[0], v[1], v[2], v[3]); }
            "C" => {
                let v = [num(&mut it)?, num(&mut it)?, num(&mut it)?, num(&mut it)?, num(&mut it)?, num(&mut it)?];
                pb.cubic_to(v[0], v[1], v[2], v[3], v[4], v[5]);
            }
            "Z" => pb.close(),
            _ => return None,
        }
    }
    pb.finish()
}

fn color(text: &str, alpha: f32) -> tiny_skia::Color {
    let c = crate::css::parse_color(text.trim()).unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 255 });
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, ((c.a as f32) * alpha.clamp(0.0, 1.0)).round() as u8)
}

fn stops(text: &str, alpha: f32) -> Vec<GradientStop> {
    text.split('|')
        .filter_map(|s| s.split_once(','))
        .filter_map(|(o, c)| o.trim().parse::<f32>().ok().map(|o| GradientStop::new(o.clamp(0.0, 1.0), color(c, alpha))))
        .collect()
}

/// A paint from the prelude's style spec: `c:<css colour>`,
/// `l:x0,y0,x1,y1;stops`, `r:x0,y0,r0,x1,y1,r1;stops`, `p:<image key>;repeat`
/// (stops are `offset,colour|…`), drawn with `paint_transform` (the CTM).
fn paint<'a>(spec: &str, paint_transform: Transform, alpha: f32, op: &str, images: &'a mut Vec<Pixmap>) -> Paint<'a> {
    let mut p = Paint { anti_alias: true, blend_mode: blend(op), ..Paint::default() };
    let (kind, rest) = spec.split_once(':').unwrap_or(("c", spec));
    p.shader = match kind {
        "l" | "r" => {
            let (geo, list) = rest.split_once(';').unwrap_or((rest, ""));
            let g = numbers(geo);
            let mut st = stops(list, alpha);
            if st.is_empty() {
                return transparent(p);
            }
            if st.len() == 1 {
                let only = st[0].clone();
                st.push(only);
            }
            let shader = if kind == "l" && g.len() == 4 {
                LinearGradient::new(Point::from_xy(g[0], g[1]), Point::from_xy(g[2], g[3]), st, SpreadMode::Pad, paint_transform)
            } else if kind == "r" && g.len() == 6 {
                RadialGradient::new(Point::from_xy(g[0], g[1]), Point::from_xy(g[3], g[4]), g[5].max(0.001), st, SpreadMode::Pad, paint_transform)
            } else {
                None
            };
            match shader {
                Some(s) => s,
                // a degenerate gradient paints its last colour
                None => Shader::SolidColor(color(list.rsplit('|').next().and_then(|s| s.split_once(',')).map(|(_, c)| c).unwrap_or("transparent"), alpha)),
            }
        }
        "p" => {
            let (key, repeat) = rest.split_once(';').unwrap_or((rest, "repeat"));
            match source(key) {
                Some(pix) => {
                    images.push(pix);
                    let pix = images.last().expect("just pushed");
                    let spread = if repeat == "no-repeat" { SpreadMode::Pad } else { SpreadMode::Repeat };
                    Pattern::new(pix.as_ref(), spread, FilterQuality::Bilinear, alpha.clamp(0.0, 1.0), paint_transform)
                }
                None => return transparent(p),
            }
        }
        _ => Shader::SolidColor(color(rest, alpha)),
    };
    p
}

fn transparent(mut p: Paint) -> Paint {
    p.shader = Shader::SolidColor(tiny_skia::Color::TRANSPARENT);
    p
}

fn blend(op: &str) -> BlendMode {
    match op {
        "copy" => BlendMode::Source,
        "clear" => BlendMode::Clear,
        "source-in" => BlendMode::SourceIn,
        "source-out" => BlendMode::SourceOut,
        "source-atop" => BlendMode::SourceAtop,
        "destination-over" => BlendMode::DestinationOver,
        "destination-in" => BlendMode::DestinationIn,
        "destination-out" => BlendMode::DestinationOut,
        "destination-atop" => BlendMode::DestinationAtop,
        "lighter" => BlendMode::Plus,
        "xor" => BlendMode::Xor,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => BlendMode::SourceOver,
    }
}

/// An image source: another canvas (`sepia-canvas:<id>`) or a loaded image by URL.
fn source(key: &str) -> Option<Pixmap> {
    if let Some(id) = key.strip_prefix("sepia-canvas:").and_then(|n| n.parse::<usize>().ok()) {
        return snapshot(id);
    }
    let bitmap = crate::js::image_for(key)?;
    let size = tiny_skia::IntSize::from_wh(bitmap.width as u32, bitmap.height as u32)?;
    Pixmap::from_vec(bitmap.rgba.clone(), size)
}

/// Natural size of an image source (0 × 0 when it is not loaded).
pub fn source_size(key: &str) -> (u32, u32) {
    if let Some(id) = key.strip_prefix("sepia-canvas:").and_then(|n| n.parse::<usize>().ok()) {
        return SURFACES.with(|s| s.borrow().get(&id).map(|x| (x.pixmap.width(), x.pixmap.height())).unwrap_or((0, 0)));
    }
    crate::js::image_for(key).map(|b| (b.width as u32, b.height as u32)).unwrap_or((0, 0))
}

// ----- drawing ----------------------------------------------------------------

pub fn fill(id: usize, path_text: &str, evenodd: bool, spec: &str, paint_t: &str, alpha: f32, op: &str) {
    let Some(path) = path(path_text) else { return };
    let mut keep = Vec::new();
    let p = paint(spec, transform(paint_t), alpha, op, &mut keep);
    let rule = if evenodd { FillRule::EvenOdd } else { FillRule::Winding };
    with_surface(id, |s| s.pixmap.fill_path(&path, &p, rule, Transform::identity(), s.clip.as_ref()));
}

#[allow(clippy::too_many_arguments)]
pub fn stroke(id: usize, path_text: &str, spec: &str, paint_t: &str, alpha: f32, op: &str, width: f32, cap: &str, join: &str, miter: f32, dash: &str, dash_offset: f32) {
    let Some(path) = path(path_text) else { return };
    let mut keep = Vec::new();
    let p = paint(spec, transform(paint_t), alpha, op, &mut keep);
    let mut st = Stroke { width: width.max(0.0), miter_limit: miter.max(1.0), ..Stroke::default() };
    st.line_cap = match cap { "round" => tiny_skia::LineCap::Round, "square" => tiny_skia::LineCap::Square, _ => tiny_skia::LineCap::Butt };
    st.line_join = match join { "round" => tiny_skia::LineJoin::Round, "bevel" => tiny_skia::LineJoin::Bevel, _ => tiny_skia::LineJoin::Miter };
    let mut d = numbers(dash);
    if !d.is_empty() {
        if d.len() % 2 == 1 {
            d.extend(d.clone());
        }
        st.dash = StrokeDash::new(d, dash_offset);
    }
    if st.width == 0.0 {
        st.width = 0.0001;
    }
    with_surface(id, |s| s.pixmap.stroke_path(&path, &p, &st, Transform::identity(), s.clip.as_ref()));
}

/// The clip: `rule;path` entries separated by `#`, intersected ("" clears it).
pub fn clip(id: usize, list: &str) {
    with_surface(id, |s| {
        let (w, h) = (s.pixmap.width(), s.pixmap.height());
        let mut mask: Option<Mask> = None;
        for entry in list.split('#').filter(|e| !e.trim().is_empty()) {
            let (rule, text) = entry.split_once(';').unwrap_or(("nonzero", entry));
            let rule = if rule == "evenodd" { FillRule::EvenOdd } else { FillRule::Winding };
            match (path(text), mask.as_mut()) {
                (Some(p), Some(m)) => m.intersect_path(&p, rule, true, Transform::identity()),
                (Some(p), None) => {
                    let mut m = Mask::new(w, h).expect("canvas size");
                    m.fill_path(&p, rule, true, Transform::identity());
                    mask = Some(m);
                }
                // an empty clip path clips everything away
                (None, _) => mask = Some(Mask::new(w, h).expect("canvas size")),
            }
        }
        s.clip = mask;
    });
}

/// drawImage: the source rectangle (sx, sy, sw, sh) of `key`, mapped by `t`
/// (CTM × destination placement) onto the canvas.
#[allow(clippy::too_many_arguments)]
pub fn draw_image(id: usize, key: &str, sx: f32, sy: f32, sw: f32, sh: f32, t: &str, alpha: f32, op: &str, smooth: bool) {
    let Some(src) = source(key) else { return };
    let Some(rect) = Rect::from_xywh(sx, sy, sw, sh) else { return };
    let quality = if smooth { FilterQuality::Bilinear } else { FilterQuality::Nearest };
    let p = Paint {
        shader: Pattern::new(src.as_ref(), SpreadMode::Pad, quality, alpha.clamp(0.0, 1.0), Transform::identity()),
        blend_mode: blend(op),
        anti_alias: true,
        ..Paint::default()
    };
    let t = transform(t);
    with_surface(id, |s| s.pixmap.fill_rect(rect, &p, t, s.clip.as_ref()));
}

/// Width of `text` in px.
pub fn measure(fonts: &Fonts, text: &str, size: f32, family: &str, bold: bool, italic: bool) -> f32 {
    let family = (!family.trim().is_empty()).then_some(family);
    fonts.advance_in(text, size, bold, italic, family)
}

/// fillText / strokeText (stroked text is filled thinly: glyph outlines are
/// not exposed, so it reads as text rather than vanishing). `x`, `y` are the
/// alphabetic baseline start in user space; `ctm` places it.
#[allow(clippy::too_many_arguments)]
pub fn text(fonts: &Fonts, id: usize, text: &str, x: f32, y: f32, size: f32, family: &str, bold: bool, italic: bool, max_width: f32, spec: &str, ctm: &str, alpha: f32, op: &str) {
    let ctm_t = transform(ctm);
    // the scale the CTM gives text (rotation is not drawn)
    let scale = (ctm_t.sx * ctm_t.sy - ctm_t.kx * ctm_t.ky).abs().sqrt().max(0.0001);
    let family_opt = (!family.trim().is_empty()).then_some(family);
    let natural = fonts.advance_in(text, size, bold, italic, family_opt);
    let squeeze = if max_width > 0.0 && natural > max_width { max_width / natural } else { 1.0 };
    let origin = (ctm_t.sx * x + ctm_t.kx * y + ctm_t.tx, ctm_t.ky * x + ctm_t.sy * y + ctm_t.ty);
    let px = size * scale;
    let Some((w, h)) = SURFACES.with(|s| s.borrow().get(&id).map(|s| (s.pixmap.width(), s.pixmap.height()))) else { return };
    let Some(mut mask) = Mask::new(w, h) else { return };
    let mut pen = origin.0;
    let mut any = false;
    {
        let data = mask.data_mut();
        fonts.for_each_glyph_in(family_opt, text, bold, italic, |glyph| {
            let advance = glyph.advance(px) * squeeze;
            if let Glyph::Outline(font, ch) = glyph {
                let (m, cov) = font.rasterize(ch, px);
                let gx = (pen + m.xmin as f32).round() as i64;
                let gy = (origin.1 - (m.ymin as f32 + m.height as f32)).round() as i64;
                for row in 0..m.height {
                    let yy = gy + row as i64;
                    if yy < 0 || yy >= h as i64 {
                        continue;
                    }
                    for col in 0..m.width {
                        let xx = gx + col as i64;
                        if xx < 0 || xx >= w as i64 {
                            continue;
                        }
                        let v = cov[row * m.width + col];
                        if v > 0 {
                            let cell = &mut data[yy as usize * w as usize + xx as usize];
                            *cell = (*cell).max(v);
                            any = true;
                        }
                    }
                }
            }
            pen += advance;
        });
    }
    if !any {
        return;
    }
    let mut keep = Vec::new();
    let p = paint(spec, ctm_t, alpha, op, &mut keep);
    with_surface(id, |s| {
        if let Some(clip) = &s.clip {
            for (a, c) in mask.data_mut().iter_mut().zip(clip.data()) {
                *a = ((*a as u16 * *c as u16) / 255) as u8;
            }
        }
        if let Some(full) = Rect::from_xywh(0.0, 0.0, w as f32, h as f32) {
            s.pixmap.fill_rect(full, &p, Transform::identity(), Some(&mask));
        }
    });
}

/// getImageData: straight (not premultiplied) RGBA of a rectangle, one
/// byte per char (latin-1), transparent outside the canvas.
pub fn get_image_data(id: usize, x: i32, y: i32, w: u32, h: u32) -> String {
    let w = w.clamp(1, 16384);
    let h = h.clamp(1, 16384);
    let mut out = vec![0u8; (w * h * 4) as usize];
    SURFACES.with(|s| {
        if let Some(surf) = s.borrow().get(&id) {
            let (pw, ph) = (surf.pixmap.width() as i32, surf.pixmap.height() as i32);
            let data = surf.pixmap.data();
            for row in 0..h as i32 {
                let sy = y + row;
                if sy < 0 || sy >= ph {
                    continue;
                }
                for col in 0..w as i32 {
                    let sx = x + col;
                    if sx < 0 || sx >= pw {
                        continue;
                    }
                    let i = ((sy * pw + sx) * 4) as usize;
                    let o = ((row * w as i32 + col) * 4) as usize;
                    let a = data[i + 3];
                    for k in 0..3 {
                        out[o + k] = if a == 0 { 0 } else { ((data[i + k] as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
                    }
                    out[o + 3] = a;
                }
            }
        }
    });
    out.into_iter().map(|b| b as char).collect()
}

/// putImageData: straight RGBA bytes (latin-1 chars) written as they are.
pub fn put_image_data(id: usize, bytes: &str, dx: i32, dy: i32, w: u32, h: u32) {
    let src: Vec<u8> = bytes.chars().map(|c| c as u32 as u8).collect();
    with_surface(id, |s| {
        let (pw, ph) = (s.pixmap.width() as i32, s.pixmap.height() as i32);
        let data = s.pixmap.data_mut();
        for row in 0..h as i32 {
            let ty = dy + row;
            if ty < 0 || ty >= ph {
                continue;
            }
            for col in 0..w as i32 {
                let tx = dx + col;
                if tx < 0 || tx >= pw {
                    continue;
                }
                let i = ((row * w as i32 + col) * 4) as usize;
                if i + 3 >= src.len() {
                    return;
                }
                let o = ((ty * pw + tx) * 4) as usize;
                let a = src[i + 3] as u32;
                for k in 0..3 {
                    data[o + k] = ((src[i + k] as u32 * a + 127) / 255) as u8;
                }
                data[o + 3] = a as u8;
            }
        }
    });
}

/// toDataURL: the canvas as a PNG, base64.
pub fn png_base64(id: usize) -> String {
    let Some((w, h)) = SURFACES.with(|s| s.borrow().get(&id).map(|x| (x.pixmap.width(), x.pixmap.height()))) else { return String::new() };
    let rgba: Vec<u8> = get_image_data(id, 0, 0, w, h).chars().map(|c| c as u32 as u8).collect();
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let ok = enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba));
        if ok.is_err() {
            return String::new();
        }
    }
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_reads_back() {
        init(9001, 10, 10);
        fill(9001, "M 2 2 L 8 2 L 8 8 L 2 8 Z", false, "c:rgb(255, 0, 0)", "1,0,0,1,0,0", 1.0, "source-over");
        let px = get_image_data(9001, 5, 5, 1, 1);
        let b: Vec<u8> = px.chars().map(|c| c as u8).collect();
        assert_eq!(b, vec![255, 0, 0, 255]);
        let corner: Vec<u8> = get_image_data(9001, 0, 0, 1, 1).chars().map(|c| c as u8).collect();
        assert_eq!(corner, vec![0, 0, 0, 0]);
        assert!(png_base64(9001).starts_with("iVBORw0KGgo"));
        assert!(take_dirty().iter().any(|(k, b)| k == "sepia-canvas:9001" && b.width == 10));
    }

    #[test]
    fn clips_and_clears() {
        init(9002, 10, 10);
        clip(9002, "nonzero;M 0 0 L 5 0 L 5 10 L 0 10 Z");
        fill(9002, "M 0 0 L 10 0 L 10 10 L 0 10 Z", false, "c:blue", "1,0,0,1,0,0", 1.0, "source-over");
        let get = |x| get_image_data(9002, x, 5, 1, 1).chars().map(|c| c as u8).collect::<Vec<u8>>();
        assert_eq!(get(2)[3], 255);
        assert_eq!(get(8)[3], 0, "the clip keeps the right half empty");
        clip(9002, "");
        fill(9002, "M 0 0 L 10 0 L 10 10 L 0 10 Z", false, "c:#000", "1,0,0,1,0,0", 1.0, "clear");
        assert_eq!(get(2)[3], 0);
    }
}
