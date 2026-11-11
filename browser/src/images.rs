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
const MAX_PIXELS: usize = 16_000_000;
const MAX_SIDE: usize = 8192;

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
    fn new(width: usize, height: usize, rgba: Vec<u8>) -> Bitmap {
        Bitmap { width, height, rgba, natural: None, scaled: Mutex::new(None) }
    }

    pub fn from_premultiplied(width: usize, height: usize, rgba: Vec<u8>) -> Bitmap {
        Bitmap::new(width, height, rgba)
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
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        decode_gif(bytes)
    } else if crate::svg::sniff(bytes) {
        crate::svg::decode(bytes)
    } else {
        None
    }
}

fn fits(w: usize, h: usize) -> bool {
    w > 0 && h > 0 && w <= MAX_SIDE && h <= MAX_SIDE && w * h <= MAX_PIXELS
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
    fn rejects_garbage_and_huge() {
        assert!(decode(b"<html>not an image</html>").is_none());
        assert!(!fits(100_000, 1));
    }

    #[test]
    fn shrinking_averages() {
        // 2x1 black|white -> 1x1 grey
        let out = resample(&[0, 0, 0, 255, 255, 255, 255, 255], 2, 1, 1, 1);
        assert_eq!(out, vec![128, 128, 128, 255]);
    }
}
