//! Images from the document: decoding (PNG, JPEG, GIF first frame, data: URIs)
//! and scaling for paint.
//!
//! Nothing here touches the network: `page.rs` fetches the bytes through the
//! same `net::fetch_with` path as the page and hands them over. Decoded
//! pixels live in memory only.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

/// Largest image we decode, in pixels (a 4K photo is ~8.3 M).
const MAX_PIXELS: usize = usize::MAX;
const MAX_SIDE: usize = usize::MAX;

/// A decoded image, premultiplied RGBA8.
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// Size in CSS px when it differs from the pixel size (SVG is rendered
    /// larger than its natural size so it stays sharp).
    pub natural: Option<(f32, f32)>,
    /// Last scaled copy handed to paint, so a scrolled page does not
    /// resample a large photo every frame.
    scaled: Mutex<Option<(usize, usize, Arc<Vec<u8>>)>>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bitmap({}x{})", self.width, self.height)
    }
}

/// Images of one page, keyed by the `src` attribute exactly as written in the document.
pub type ImageMap = HashMap<String, Arc<Bitmap>>;

impl Bitmap {
    pub(crate) fn new(width: usize, height: usize, rgba: Vec<u8>) -> Bitmap {
        Bitmap { width, height, rgba, natural: None, scaled: Mutex::new(None) }
    }

    pub fn from_premultiplied(width: usize, height: usize, rgba: Vec<u8>) -> Bitmap {
        Bitmap::new(width, height, rgba)
    }

    /// This image used as a mask (CSS mask-image): its alpha, filled with `rgba`.
    pub fn tinted(&self, rgba: [u8; 4]) -> Bitmap {
        let mut out = Vec::with_capacity(self.rgba.len());
        for px in self.rgba.chunks_exact(4) {
            let a = (px[3] as u32 * rgba[3] as u32 / 255) as u8;
            // premultiplied, as every bitmap here
            out.extend_from_slice(&[(rgba[0] as u32 * a as u32 / 255) as u8, (rgba[1] as u32 * a as u32 / 255) as u8, (rgba[2] as u32 * a as u32 / 255) as u8, a]);
        }
        let mut b = Bitmap::new(self.width, self.height, out);
        b.natural = self.natural;
        b
    }

    /// The pixels inside (x, y, w, h), clamped to the bitmap; crops are
    /// remembered, since the same background is laid out again and again.
    pub fn crop(self: &Arc<Self>, x: usize, y: usize, w: usize, h: usize) -> Arc<Bitmap> {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let w = w.clamp(1, self.width - x);
        let h = h.clamp(1, self.height - y);
        if (x, y, w, h) == (0, 0, self.width, self.height) {
            return self.clone();
        }
        static CROPS: std::sync::OnceLock<Mutex<Vec<((usize, usize, usize, usize, usize), Arc<Bitmap>)>>> = std::sync::OnceLock::new();
        let key = (Arc::as_ptr(self) as usize, x, y, w, h);
        let cache = CROPS.get_or_init(Default::default);
        if let Some((_, hit)) = cache.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) {
            return hit.clone();
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in y..y + h {
            let start = (row * self.width + x) * 4;
            rgba.extend_from_slice(&self.rgba[start..start + w * 4]);
        }
        let out = Arc::new(Bitmap::new(w, h, rgba));
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 64 {
            cache.remove(0);
        }
        cache.push((key, out.clone()));
        out
    }

    /// Size for layout, in CSS px.
    pub fn natural_size(&self) -> (f32, f32) {
        self.natural.unwrap_or((self.width as f32, self.height as f32))
    }

    /// From straight (non-premultiplied) RGBA.
    fn from_straight(width: usize, height: usize, mut rgba: Vec<u8>) -> Bitmap {
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
        }
        Bitmap::new(width, height, rgba)
    }

    /// This image at `w`×`h` device px (premultiplied RGBA), cached for the last size asked.
    pub fn scaled(&self, w: usize, h: usize) -> Arc<Vec<u8>> {
        let mut slot = self.scaled.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((sw, sh, data)) = slot.as_ref() {
            if *sw == w && *sh == h {
                return data.clone();
            }
        }
        let data = Arc::new(resample(&self.rgba, self.width, self.height, w, h));
        *slot = Some((w, h, data.clone()));
        data
    }
}

/// Decode by content, not by extension or Content-Type: servers lie.
pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    let bitmap = decode_known(bytes);
    if bitmap.is_none() && !bytes.is_empty() {
        crate::report::miss("image", sniff_name(bytes));
    }
    bitmap
}

/// What an image file is, by its first bytes (for the sensors).
fn sniff_name(bytes: &[u8]) -> &'static str {
    let at = |i: usize, s: &[u8]| bytes.get(i..i + s.len()) == Some(s);
    if at(4, b"ftypavif") || at(4, b"ftypavis") {
        "avif"
    } else if at(4, b"ftypheic") || at(4, b"ftypmif1") {
        "heic"
    } else if at(0, &[0xFF, 0x0A]) || at(4, b"JXL ") {
        "jxl"
    } else if at(0, b"BM") {
        "bmp"
    } else if at(0, b"\x89PNG") {
        "png (broken)"
    } else if at(0, &[0xFF, 0xD8]) {
        "jpeg (broken)"
    } else if at(0, b"GIF8") {
        "gif (broken)"
    } else if at(0, b"RIFF") {
        "webp (broken)"
    } else if bytes.iter().take(64).any(|&b| b == b'<') {
        "markup (svg or html)"
    } else {
        "unknown"
    }
}

fn decode_known(bytes: &[u8]) -> Option<Bitmap> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        decode_gif(bytes)
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        decode_webp(bytes)
    } else if crate::svg::sniff(bytes) {
        crate::svg::decode(bytes)
    } else if bytes.starts_with(&[0, 0, 1, 0]) {
        decode_ico(bytes)
    } else {
        None
    }
}

/// Windows icons (favicon.ico): the largest image up to 64 px, either a PNG
/// inside or a BMP (32/24/8/4/1 bit, bottom-up, with its AND mask).
fn decode_ico(bytes: &[u8]) -> Option<Bitmap> {
    let u16le = |at: usize| -> Option<usize> { Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize) };
    let u32le = |at: usize| -> Option<usize> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize) };
    let count = u16le(4)?.min(64);
    let mut best: Option<(usize, usize, usize)> = None; // (size, offset, length)
    for i in 0..count {
        let e = 6 + i * 16;
        let w = match *bytes.get(e)? { 0 => 256, w => w as usize };
        let (len, off) = (u32le(e + 8)?, u32le(e + 12)?);
        // prefer the biggest up to 64 px, else the smallest bigger one
        let rank = |s: usize| if s <= 64 { s } else { 1000 - s.min(999) };
        if best.is_none_or(|(b, _, _)| rank(w) > rank(b)) {
            best = Some((w, off, len));
        }
    }
    let (_, off, len) = best?;
    let data = bytes.get(off..off.checked_add(len)?)?;
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return decode_png(data);
    }
    // BITMAPINFOHEADER; the height counts the XOR image and the AND mask
    let d16 = |at: usize| -> Option<usize> { Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?) as usize) };
    let d32 = |at: usize| -> Option<i64> { Some(i32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as i64) };
    let header = d32(0)? as usize;
    let w = d32(4)?.unsigned_abs() as usize;
    let h = (d32(8)?.unsigned_abs() as usize) / 2;
    let bpp = d16(14)?;
    if !fits(w, h) || w == 0 || h == 0 || header < 40 {
        return None;
    }
    let colors = match d32(32)? { 0 if bpp <= 8 => 1 << bpp, n => n.max(0) as usize };
    let palette_at = header;
    let pixels_at = palette_at + if bpp <= 8 { colors * 4 } else { 0 };
    let stride = (w * bpp).div_ceil(32) * 4;
    let mask_at = pixels_at + stride * h;
    let mask_stride = w.div_ceil(32) * 4;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = pixels_at + (h - 1 - y) * stride;
        for x in 0..w {
            let (b, g, r, a) = match bpp {
                32 => { let p = data.get(row + x * 4..row + x * 4 + 4)?; (p[0], p[1], p[2], p[3]) }
                24 => { let p = data.get(row + x * 3..row + x * 3 + 3)?; (p[0], p[1], p[2], 255) }
                8 | 4 | 1 => {
                    let bit = x * bpp;
                    let byte = *data.get(row + bit / 8)?;
                    let index = ((byte >> (8 - bpp - bit % 8)) & ((1u16 << bpp) - 1) as u8) as usize;
                    let p = data.get(palette_at + index * 4..palette_at + index * 4 + 4)?;
                    (p[0], p[1], p[2], 255)
                }
                _ => return None,
            };
            // the AND mask hides pixels (32-bit icons carry alpha instead)
            let hidden = bpp != 32 && data.get(mask_at + (h - 1 - y) * mask_stride + x / 8).is_some_and(|m| m & (0x80 >> (x % 8)) != 0);
            let o = (y * w + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[r, g, b, if hidden { 0 } else { a }]);
        }
    }
    Some(Bitmap::from_straight(w, h, rgba))
}

fn decode_webp(bytes: &[u8]) -> Option<Bitmap> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).ok()?;
    let (w, h) = decoder.dimensions();
    let (w, h) = (w as usize, h as usize);
    if !fits(w, h) {
        return None;
    }
    let alpha = decoder.has_alpha();
    let mut buf = vec![0u8; decoder.output_buffer_size()?];
    // animated WebP: the first frame
    decoder.read_image(&mut buf).ok()?;
    let rgba = if alpha {
        buf
    } else {
        let mut out = Vec::with_capacity(w * h * 4);
        for px in buf.chunks_exact(3) {
            out.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        out
    };
    Some(Bitmap::from_straight(w, h, rgba))
}

fn fits(w: usize, h: usize) -> bool {
    w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE && w.checked_mul(h).is_some_and(|p| p <= MAX_PIXELS)
}

fn decode_png(bytes: &[u8]) -> Option<Bitmap> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = {
        let info = reader.info();
        (info.width as usize, info.height as usize)
    };
    if !fits(w, h) {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &buf[y * info.line_size..];
        for x in 0..w {
            let p = &row[x * channels..x * channels + channels];
            out.extend_from_slice(&match channels {
                4 => [p[0], p[1], p[2], p[3]],
                3 => [p[0], p[1], p[2], 255],
                2 => [p[0], p[0], p[0], p[1]],
                _ => [p[0], p[0], p[0], 255],
            });
        }
    }
    Some(Bitmap::from_straight(w, h, out))
}

fn decode_jpeg(bytes: &[u8]) -> Option<Bitmap> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().ok()?;
    let info = decoder.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if !fits(w, h) {
        return None;
    }
    let pixels = decoder.decode().ok()?;
    (pixels.len() == w * h * 4).then(|| Bitmap::new(w, h, pixels))
}

/// GIF: the first frame only (no animation), composited onto a transparent canvas.
fn decode_gif(bytes: &[u8]) -> Option<Bitmap> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options.read_info(Cursor::new(bytes)).ok()?;
    let (w, h) = (decoder.width() as usize, decoder.height() as usize);
    if !fits(w, h) {
        return None;
    }
    let frame = decoder.read_next_frame().ok()??;
    let mut out = vec![0u8; w * h * 4];
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    let (fl, ft) = (frame.left as usize, frame.top as usize);
    for y in 0..fh {
        for x in 0..fw {
            let (dx, dy) = (fl + x, ft + y);
            if dx >= w || dy >= h {
                continue;
            }
            let s = (y * fw + x) * 4;
            let d = (dy * w + dx) * 4;
            if let Some(px) = frame.buffer.get(s..s + 4) {
                out[d..d + 4].copy_from_slice(px);
            }
        }
    }
    Some(Bitmap::from_straight(w, h, out))
}

/// `data:image/png;base64,....` — decoded locally, no request.
/// The bytes of a `data:` URL (base64 or percent-encoded) and its media type.
pub fn data_url_bytes(src: &str) -> Option<(String, Vec<u8>)> {
    let rest = src.trim().strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let kind = meta.split(';').next().unwrap_or("").to_ascii_lowercase();
    if meta.to_ascii_lowercase().ends_with(";base64") {
        return Some((kind, base64(&String::from_utf8_lossy(&percent_decode(payload)))?));
    }
    Some((kind, percent_decode(payload)))
}

pub fn decode_data_uri(src: &str) -> Option<Bitmap> {
    let rest = src.trim().strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        return decode(&base64(payload)?);
    }
    // URL-encoded text, the usual way to inline an SVG
    decode(&percent_decode(payload))
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in input.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Scale premultiplied RGBA. Shrinking averages every source pixel under
/// the destination pixel (no aliasing on photos); growing is bilinear.
pub fn resample(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return out;
    }
    if sw == dw && sh == dh {
        out.copy_from_slice(src);
        return out;
    }
    let (fx, fy) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
    if fx >= 1.0 && fy >= 1.0 {
        for y in 0..dh {
            let y0 = (y as f32 * fy) as usize;
            let y1 = (((y + 1) as f32 * fy).ceil() as usize).clamp(y0 + 1, sh);
            for x in 0..dw {
                let x0 = (x as f32 * fx) as usize;
                let x1 = (((x + 1) as f32 * fx).ceil() as usize).clamp(x0 + 1, sw);
                let mut sum = [0u32; 4];
                for sy in y0..y1 {
                    let row = sy * sw;
                    for sx in x0..x1 {
                        let i = (row + sx) * 4;
                        for c in 0..4 {
                            sum[c] += src[i + c] as u32;
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                for c in 0..4 {
                    out[(y * dw + x) * 4 + c] = ((sum[c] + n / 2) / n) as u8;
                }
            }
        }
        return out;
    }
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (sx.floor() as usize, sx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            for c in 0..4 {
                let p = |xx: usize, yy: usize| src[(yy * sw + xx) * 4 + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bottom = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                out[(y * dw + x) * 4 + c] = (top * (1.0 - ty) + bottom * ty).round() as u8;
            }
        }
    }
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&rgba.repeat((w * h) as usize)).unwrap();
        }
        out
    }

    #[test]
    fn decodes_bmp_icons_with_alpha_and_mask() {
        // 2x2, 32 bpp (alpha in the pixels) and 24 bpp (alpha from the AND mask)
        fn ico(bpp: u16, pixels: &[u8], mask: &[u8]) -> Vec<u8> {
            let mut dib = Vec::new();
            for v in [40u32, 2, 4] { dib.extend(v.to_le_bytes()); }
            dib.extend(1u16.to_le_bytes());
            dib.extend(bpp.to_le_bytes());
            dib.extend([0u8; 24]);
            dib.extend(pixels);
            dib.extend(mask);
            let mut out = vec![0, 0, 1, 0, 1, 0, 2, 2, 0, 0, 1, 0];
            out.extend(bpp.to_le_bytes());
            out.extend((dib.len() as u32).to_le_bytes());
            out.extend(22u32.to_le_bytes());
            out.extend(dib);
            out
        }
        // bottom-up rows: the first row in the file is the bottom one
        let b32 = ico(32, &[0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0], &[0; 8]);
        let bitmap = decode(&b32).expect("32-bit icon");
        assert_eq!((bitmap.width, bitmap.height), (2, 2));
        assert_eq!(&bitmap.rgba[0..4], &[0, 0, 255, 255], "top-left is blue");
        assert_eq!(&bitmap.rgba[12..16], &[0, 255, 0, 255], "bottom-right is green");
        assert_eq!(bitmap.rgba[7], 0, "transparent pixel");
        // 24 bpp rows pad to 4 bytes; mask bit 1 hides the top-left pixel
        let b24 = ico(24, &[1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0], &[0, 0, 0, 0, 0x80, 0, 0, 0]);
        let bitmap = decode(&b24).expect("24-bit icon");
        assert_eq!(bitmap.rgba[3], 0);
        assert_eq!(bitmap.rgba[7], 255);
    }

    #[test]
    fn decodes_png_premultiplied() {
        let bmp = decode(&png_bytes(3, 2, [200, 100, 0, 128])).expect("png");
        assert_eq!((bmp.width, bmp.height), (3, 2));
        assert_eq!(&bmp.rgba[0..4], &[100, 50, 0, 128]);
    }

    #[test]
    fn decodes_gif_first_frame() {
        let mut bytes = Vec::new();
        {
            let palette = [255u8, 0, 0, 0, 0, 255];
            let mut enc = gif::Encoder::new(&mut bytes, 2, 2, &palette).unwrap();
            let frame = gif::Frame { width: 2, height: 2, buffer: std::borrow::Cow::Borrowed(&[0, 1, 1, 0]), ..Default::default() };
            enc.write_frame(&frame).unwrap();
        }
        let bmp = decode(&bytes).expect("gif");
        assert_eq!((bmp.width, bmp.height), (2, 2));
        assert_eq!(&bmp.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&bmp.rgba[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn decodes_jpeg() {
        // docs/fixtures/img/gradient.jpg: macOS `sips` export of gradient.png
        // (red rises left to right, green top to bottom, blue 160).
        let bmp = decode(include_bytes!("../../docs/fixtures/img/gradient.jpg")).expect("jpeg");
        assert_eq!((bmp.width, bmp.height), (320, 200));
        let px = |x: usize, y: usize| &bmp.rgba[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4];
        let (tl, br) = (px(2, 2), px(317, 197));
        assert!(tl[0] < 40 && tl[1] < 40 && (tl[2] as i32 - 160).abs() < 25, "top-left {tl:?}");
        assert!(br[0] > 215 && br[1] > 215, "bottom-right {br:?}");
        assert_eq!(tl[3], 255);
    }

    #[test]
    fn data_uri_is_decoded_locally() {
        let png = png_bytes(1, 1, [0, 255, 0, 255]);
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for chunk in png.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    b64.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    b64.push('=');
                }
            }
        }
        let bmp = decode_data_uri(&format!("data:image/png;base64,{b64}")).expect("data uri");
        assert_eq!(&bmp.rgba[..], &[0, 255, 0, 255]);
        assert!(decode_data_uri("data:text/plain,hello").is_none());
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"<html>not an image</html>").is_none());
        assert!(fits(100_000, 1) && !fits(0, 1));
    }

    #[test]
    fn shrinking_averages() {
        // 2x1 black|white -> 1x1 grey
        let out = resample(&[0, 0, 0, 255, 255, 255, 255, 255], 2, 1, 1, 1);
        assert_eq!(out, vec![128, 128, 128, 255]);
    }
}

/// The viewport width responsive images are chosen for (set per page).
static VIEWPORT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1200);

pub fn set_viewport(width: f32) {
    VIEWPORT.store(width.max(1.0) as u32, std::sync::atomic::Ordering::Relaxed);
}

/// The address an `<img>` shows, chosen as a browser does: from the
/// `<source>`s of its `<picture>` (the first whose media matches and whose
/// type this browser decodes), else its own srcset, else its src. With `w`
/// descriptors the smallest candidate at least as wide as the slot `sizes`
/// gives (at 1 device pixel per CSS pixel); with `x`, the 1x one.
/// The page loader and the layout both ask this, so they agree on the key.
pub fn chosen_src<'n>(img: &'n crate::dom::Node, parent: Option<&'n crate::dom::Node>) -> Option<&'n str> {
    let viewport = VIEWPORT.load(std::sync::atomic::Ordering::Relaxed) as f32;
    let sizes = img.attr("sizes");
    if let Some(picture) = parent.filter(|p| p.tag == "picture") {
        for source in picture.children.iter().filter(|c| c.tag == "source") {
            let media_ok = source.attr("media").is_none_or(|m| crate::css::Media::parse(m).matches_viewport(viewport, 800.0));
            let type_ok = source.attr("type").is_none_or(|t| decodable_type(t));
            if media_ok && type_ok {
                if let Some(pick) = source.attr("srcset").and_then(|set| pick_candidate(set, source.attr("sizes").or(sizes), viewport)) {
                    return Some(pick);
                }
            }
        }
    }
    if let Some(pick) = img.attr("srcset").and_then(|set| pick_candidate(set, sizes, viewport)) {
        return Some(pick);
    }
    img.attr("src").map(str::trim).filter(|s| !s.is_empty())
}

/// Formats `decode` reads, by MIME type.
fn decodable_type(mime: &str) -> bool {
    matches!(mime.trim().to_ascii_lowercase().as_str(), "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/svg+xml" | "image/x-icon" | "image/vnd.microsoft.icon" | "")
}

fn pick_candidate<'a>(srcset: &'a str, sizes: Option<&str>, viewport: f32) -> Option<&'a str> {
    // "url 640w, url 1280w" or "url 1x, url 2x" (commas inside URLs are rare;
    // a candidate is split at ", " or a comma followed by its URL)
    let mut candidates: Vec<(&str, Option<f32>, Option<f32>)> = Vec::new();
    for part in srcset.split(',') {
        let mut words = part.split_whitespace();
        let Some(url) = words.next() else { continue };
        let (mut w, mut x) = (None, None);
        for d in words {
            if let Some(n) = d.strip_suffix('w').and_then(|n| n.parse::<f32>().ok()) {
                w = Some(n);
            } else if let Some(n) = d.strip_suffix('x').and_then(|n| n.parse::<f32>().ok()) {
                x = Some(n);
            }
        }
        candidates.push((url, w, x));
    }
    if candidates.is_empty() {
        return None;
    }
    if candidates.iter().any(|c| c.1.is_some()) {
        let slot = slot_width(sizes, viewport);
        let mut widths: Vec<(&str, f32)> = candidates.iter().filter_map(|c| c.1.map(|w| (c.0, w))).collect();
        widths.sort_by(|a, b| a.1.total_cmp(&b.1));
        return widths.iter().find(|c| c.1 >= slot).or(widths.last()).map(|c| c.0);
    }
    // density: the 1x candidate (no descriptor means 1x), else the smallest
    candidates.iter().find(|c| c.2.unwrap_or(1.0) == 1.0).or_else(|| candidates.iter().min_by(|a, b| a.2.unwrap_or(1.0).total_cmp(&b.2.unwrap_or(1.0)))).map(|c| c.0)
}

/// The slot `sizes` gives at this viewport: the first entry whose media
/// condition holds, its length in px (vw of the viewport); 100vw without one.
fn slot_width(sizes: Option<&str>, viewport: f32) -> f32 {
    let Some(sizes) = sizes else { return viewport };
    for entry in crate::css::split_top_level(sizes, ',') {
        let entry = entry.trim();
        let (condition, length) = match entry.rfind(|c: char| c.is_whitespace()) {
            Some(at) if entry.starts_with('(') => (Some(&entry[..at]), entry[at..].trim()),
            _ => (None, entry),
        };
        if condition.is_some_and(|c| !crate::css::Media::parse(c).matches_viewport(viewport, 800.0)) {
            continue;
        }
        if length == "auto" {
            return viewport;
        }
        if let Some(px) = crate::style::parse_length(length, 16.0, viewport).or_else(|| length.strip_suffix("vw").and_then(|v| v.parse::<f32>().ok()).map(|v| v * viewport / 100.0)) {
            return px;
        }
    }
    viewport
}

#[cfg(test)]
mod srcset_tests {
    use super::*;

    #[test]
    fn picks_the_candidate_a_browser_would() {
        let set = "a-320.jpg 320w, a-640.jpg 640w, a-1280.jpg 1280w, a-2400.jpg 2400w";
        assert_eq!(pick_candidate(set, Some("(max-width: 768px) 100vw, (max-width: 1180px) 50vw, 640px"), 1200.0), Some("a-640.jpg"));
        assert_eq!(pick_candidate(set, Some("(max-width: 768px) 100vw, 50vw"), 1000.0), Some("a-640.jpg"));
        assert_eq!(pick_candidate(set, None, 1200.0), Some("a-1280.jpg"));
        assert_eq!(pick_candidate(set, Some("100px"), 1200.0), Some("a-320.jpg"));
        assert_eq!(pick_candidate("b.jpg, b2.jpg 2x", None, 1200.0), Some("b.jpg"));
        assert_eq!(pick_candidate("c2.jpg 2x, c1.jpg 1x", None, 1200.0), Some("c1.jpg"));
    }
}
