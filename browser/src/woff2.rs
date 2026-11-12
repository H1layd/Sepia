//! WOFF 2.0 (W3C Recommendation) → the TrueType/OpenType font it packs.
//!
//! The table data is one Brotli stream. `glyf`/`loca` usually come
//! "transformed" (split into streams of contour counts, flags, packed
//! coordinates, bounding boxes and instructions) and `hmtx` may drop the
//! left side bearings; those are rebuilt here into plain tables. Font
//! collections are not supported. Checksums are written as 0: nothing here
//! verifies them.

const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm", b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT",
    b"EBLC", b"gasp", b"hdmx", b"kern", b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC", b"JSTF", b"MATH",
    b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar", b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar",
    b"gvar", b"hsty", b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat", b"Gloc", b"Feat", b"Sill",
];

/// A cursor over a byte slice; every read is checked.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, at: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_be_bytes(self.b.get(self.at..self.at + 2)?.try_into().ok()?);
        self.at += 2;
        Some(v)
    }
    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_be_bytes(self.b.get(self.at..self.at + 4)?.try_into().ok()?);
        self.at += 4;
        Some(v)
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let v = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(v)
    }
    /// UIntBase128: 7 bits a byte, high bit = more follows, at most 5 bytes
    fn base128(&mut self) -> Option<u32> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 0 && b == 0x80 {
                return None;
            }
            v = v.checked_mul(128)? | (b & 0x7f) as u32;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    /// 255UInt16
    fn u255(&mut self) -> Option<u16> {
        match self.u8()? {
            253 => self.u16(),
            255 => Some(253 + self.u8()? as u16),
            254 => Some(253 * 2 + self.u8()? as u16),
            c => Some(c as u16),
        }
    }
}

struct Entry {
    tag: [u8; 4],
    orig_len: usize,
    /// bytes this table takes in the decompressed stream
    stored_len: usize,
    transformed: bool,
}

/// The sfnt a WOFF2 file holds, or None when it is malformed or a collection.
pub fn to_sfnt(w: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(w);
    if r.bytes(4)? != b"wOF2" {
        return None;
    }
    let flavor = r.u32()?;
    if flavor == u32::from_be_bytes(*b"ttcf") {
        return None;
    }
    let _length = r.u32()?;
    let count = r.u16()? as usize;
    r.u16()?;
    let _total_sfnt = r.u32()?;
    let compressed_len = r.u32()? as usize;
    r.bytes(48 - 24)?; // versions, metadata and private blocks
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let flags = r.u8()?;
        let tag = match flags & 0x3f {
            63 => r.bytes(4)?.try_into().ok()?,
            i => *KNOWN_TAGS[i as usize],
        };
        let version = flags >> 6;
        let orig_len = r.base128()? as usize;
        let transformed = if &tag == b"glyf" || &tag == b"loca" { version == 0 } else { version != 0 };
        let stored_len = if transformed { r.base128()? as usize } else { orig_len };
        entries.push(Entry { tag, orig_len, stored_len, transformed });
    }
    let packed = r.bytes(compressed_len)?;
    let data = crate::net::unbrotli(packed)?;

    // slice the stream into tables
    let mut at = 0usize;
    let mut raw: Vec<(&[u8; 4], &[u8], bool, usize)> = Vec::with_capacity(count);
    for e in &entries {
        let end = at.checked_add(e.stored_len)?;
        raw.push((&e.tag, data.get(at..end)?, e.transformed, e.orig_len));
        at = end;
    }
    let find = |tag: &[u8; 4]| raw.iter().find(|t| t.0 == tag);

    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(count);
    let mut glyph_x_min: Vec<i16> = Vec::new();
    let mut rebuilt_glyf: Option<(Vec<u8>, Vec<u8>)> = None;
    if let Some((_, glyf, true, _)) = find(b"glyf") {
        let (g, l, xmins) = rebuild_glyf(glyf)?;
        glyph_x_min = xmins;
        rebuilt_glyf = Some((g, l));
    }
    for (tag, bytes, transformed, _) in &raw {
        let table = match (&tag[..], transformed) {
            (b"glyf", true) => rebuilt_glyf.as_ref()?.0.clone(),
            (b"loca", true) => rebuilt_glyf.as_ref()?.1.clone(),
            (b"hmtx", true) => {
                let hhea = find(b"hhea")?.1;
                let maxp = find(b"maxp")?.1;
                let metrics = u16::from_be_bytes(hhea.get(34..36)?.try_into().ok()?) as usize;
                let glyphs = u16::from_be_bytes(maxp.get(4..6)?.try_into().ok()?) as usize;
                rebuild_hmtx(bytes, metrics, glyphs, &glyph_x_min)?
            }
            (b"head", _) if rebuilt_glyf.is_some() => {
                // loca was rebuilt with 32-bit offsets
                let mut head = bytes.to_vec();
                head.get_mut(50..52)?.copy_from_slice(&1u16.to_be_bytes());
                head.get_mut(8..12)?.copy_from_slice(&[0; 4]);
                head
            }
            (_, true) => return None, // an unknown transform
            _ => bytes.to_vec(),
        };
        tables.push((**tag, table));
    }
    tables.sort_by(|a, b| a.0.cmp(&b.0));
    Some(assemble(flavor, &tables))
}

fn assemble(flavor: u32, tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let count = tables.len();
    let mut out = Vec::new();
    let power = (count.max(1) as f32).log2().floor() as u32;
    let range = 16u16 << power;
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&(count as u16).to_be_bytes());
    out.extend_from_slice(&range.to_be_bytes());
    out.extend_from_slice(&(power as u16).to_be_bytes());
    out.extend_from_slice(&((count as u16) * 16).saturating_sub(range).to_be_bytes());
    let mut offset = 12 + count * 16;
    for (tag, data) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += (data.len() + 3) & !3;
    }
    for (_, data) in tables {
        out.extend_from_slice(data);
        out.resize((out.len() + 3) & !3, 0);
    }
    out
}

fn with_sign(flag: u8, base: i32) -> i32 {
    if flag & 1 != 0 {
        base
    } else {
        -base
    }
}

/// One packed point: (dx, dy, on curve).
fn triplet(flag: u8, g: &mut Reader) -> Option<(i32, i32, bool)> {
    let on_curve = flag >> 7 == 0;
    let f = flag & 0x7f;
    let (dx, dy) = if f < 10 {
        (0, with_sign(f, (((f & 14) as i32) << 7) + g.u8()? as i32))
    } else if f < 20 {
        (with_sign(f, ((((f - 10) & 14) as i32) << 7) + g.u8()? as i32), 0)
    } else if f < 84 {
        let b0 = (f - 20) as i32;
        let b1 = g.u8()? as i32;
        (with_sign(f, 1 + (b0 & 0x30) + (b1 >> 4)), with_sign(f >> 1, 1 + ((b0 & 0x0c) << 2) + (b1 & 0x0f)))
    } else if f < 120 {
        let b0 = (f - 84) as i32;
        let (x, y) = (g.u8()? as i32, g.u8()? as i32);
        (with_sign(f, 1 + ((b0 / 12) << 8) + x), with_sign(f >> 1, 1 + (((b0 % 12) >> 2) << 8) + y))
    } else if f < 124 {
        let (a, b, c) = (g.u8()? as i32, g.u8()? as i32, g.u8()? as i32);
        (with_sign(f, (a << 4) + (b >> 4)), with_sign(f >> 1, ((b & 0x0f) << 8) + c))
    } else {
        let (a, b, c, d) = (g.u8()? as i32, g.u8()? as i32, g.u8()? as i32, g.u8()? as i32);
        (with_sign(f, (a << 8) + b), with_sign(f >> 1, (c << 8) + d))
    };
    Some((dx, dy, on_curve))
}

/// The transformed `glyf` → (glyf, loca with long offsets, each glyph's xMin).
fn rebuild_glyf(t: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<i16>)> {
    let mut h = Reader::new(t);
    h.u16()?;
    let option_flags = h.u16()?;
    let glyphs = h.u16()? as usize;
    let _index_format = h.u16()?;
    let mut sizes = [0usize; 7];
    for s in &mut sizes {
        *s = h.u32()? as usize;
    }
    let mut at = h.at;
    let mut next = |n: usize| -> Option<&[u8]> {
        let s = t.get(at..at.checked_add(n)?)?;
        at += n;
        Some(s)
    };
    let mut contours = Reader::new(next(sizes[0])?);
    let mut points = Reader::new(next(sizes[1])?);
    let mut flags = Reader::new(next(sizes[2])?);
    let mut glyph = Reader::new(next(sizes[3])?);
    let mut composite = Reader::new(next(sizes[4])?);
    let bbox_all = next(sizes[5])?;
    let mut instructions = Reader::new(next(sizes[6])?);
    let _ = option_flags; // the overlap bitmap that may follow is not needed
    let bitmap_len = 4 * glyphs.div_ceil(32);
    let bbox_bitmap = bbox_all.get(..bitmap_len)?;
    let mut bbox = Reader::new(bbox_all.get(bitmap_len..)?);
    let has_bbox = |i: usize| bbox_bitmap[i / 8] & (0x80 >> (i % 8)) != 0;

    let mut glyf = Vec::new();
    let mut loca = Vec::with_capacity((glyphs + 1) * 4);
    let mut x_mins = Vec::with_capacity(glyphs);
    for i in 0..glyphs {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        let n = contours.i16()?;
        if n == 0 {
            x_mins.push(0);
            continue;
        }
        let start = glyf.len();
        if n > 0 {
            let mut ends = Vec::with_capacity(n as usize);
            let mut total = 0u32;
            for _ in 0..n {
                total += points.u255()? as u32;
                ends.push(total.checked_sub(1)? as u16);
            }
            let mut pts = Vec::with_capacity(total as usize);
            let (mut x, mut y) = (0i32, 0i32);
            for _ in 0..total {
                let (dx, dy, on) = triplet(flags.u8()?, &mut glyph)?;
                x += dx;
                y += dy;
                pts.push((x, y, on));
            }
            let instr_len = glyph.u255()? as usize;
            let instr = instructions.bytes(instr_len)?;
            let (x0, y0, x1, y1) = if has_bbox(i) {
                (bbox.i16()?, bbox.i16()?, bbox.i16()?, bbox.i16()?)
            } else {
                let xs = pts.iter().map(|p| p.0);
                let ys = pts.iter().map(|p| p.1);
                (xs.clone().min().unwrap_or(0) as i16, ys.clone().min().unwrap_or(0) as i16, xs.max().unwrap_or(0) as i16, ys.max().unwrap_or(0) as i16)
            };
            x_mins.push(x0);
            for v in [n, x0, y0, x1, y1] {
                glyf.extend_from_slice(&v.to_be_bytes());
            }
            for e in &ends {
                glyf.extend_from_slice(&e.to_be_bytes());
            }
            glyf.extend_from_slice(&(instr_len as u16).to_be_bytes());
            glyf.extend_from_slice(instr);
            // every point: one flag (on curve or not), 16-bit deltas
            for p in &pts {
                glyf.push(if p.2 { 1 } else { 0 });
            }
            let mut last = 0i32;
            for p in &pts {
                glyf.extend_from_slice(&((p.0 - last) as i16).to_be_bytes());
                last = p.0;
            }
            last = 0;
            for p in &pts {
                glyf.extend_from_slice(&((p.1 - last) as i16).to_be_bytes());
                last = p.1;
            }
        } else {
            // composite: components copied as they are, bbox always explicit
            let (x0, y0, x1, y1) = (bbox.i16()?, bbox.i16()?, bbox.i16()?, bbox.i16()?);
            x_mins.push(x0);
            for v in [-1i16, x0, y0, x1, y1] {
                glyf.extend_from_slice(&v.to_be_bytes());
            }
            let begin = composite.at;
            let mut has_instructions = false;
            loop {
                let f = composite.u16()?;
                composite.u16()?;
                let mut skip = if f & 1 != 0 { 4 } else { 2 };
                if f & 0x08 != 0 {
                    skip += 2;
                } else if f & 0x40 != 0 {
                    skip += 4;
                } else if f & 0x80 != 0 {
                    skip += 8;
                }
                composite.bytes(skip)?;
                has_instructions |= f & 0x100 != 0;
                if f & 0x20 == 0 {
                    break;
                }
            }
            glyf.extend_from_slice(composite.b.get(begin..composite.at)?);
            if has_instructions {
                let len = glyph.u255()? as usize;
                glyf.extend_from_slice(&(len as u16).to_be_bytes());
                glyf.extend_from_slice(instructions.bytes(len)?);
            }
        }
        let _ = start;
        glyf.resize((glyf.len() + 3) & !3, 0);
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    Some((glyf, loca, x_mins))
}

/// The transformed `hmtx`: left side bearings left out are the glyphs' xMin.
fn rebuild_hmtx(t: &[u8], metrics: usize, glyphs: usize, x_mins: &[i16]) -> Option<Vec<u8>> {
    let mut r = Reader::new(t);
    let flags = r.u8()?;
    let mut advances = Vec::with_capacity(metrics);
    for _ in 0..metrics {
        advances.push(r.u16()?);
    }
    let x_min = |i: usize| x_mins.get(i).copied().unwrap_or(0);
    let mut lsb = Vec::with_capacity(glyphs);
    for i in 0..metrics {
        lsb.push(if flags & 1 == 0 { r.i16()? } else { x_min(i) });
    }
    for i in metrics..glyphs {
        lsb.push(if flags & 2 == 0 { r.i16()? } else { x_min(i) });
    }
    let mut out = Vec::with_capacity(metrics * 4 + (glyphs - metrics.min(glyphs)) * 2);
    for i in 0..metrics {
        out.extend_from_slice(&advances[i].to_be_bytes());
        out.extend_from_slice(&lsb[i].to_be_bytes());
    }
    for v in lsb.iter().skip(metrics) {
        out.extend_from_slice(&v.to_be_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_triplets() {
        let mut r = Reader::new(&[0x3f, 0x81, 0x00, 253, 0x01, 0x00, 255, 7, 254, 1]);
        assert_eq!(r.base128(), Some(0x3f));
        assert_eq!(r.base128(), Some(128));
        assert_eq!(r.u255(), Some(256));
        assert_eq!(r.u255(), Some(260));
        assert_eq!(r.u255(), Some(507));
        // flag 0: dy = -(0 + byte); flag 11: dx = +byte
        let mut g = Reader::new(&[5]);
        assert_eq!(triplet(0, &mut g), Some((0, -5, true)));
        let mut g = Reader::new(&[7]);
        assert_eq!(triplet(11 | 0x80, &mut g), Some((7, 0, false)));
    }

    /// A real WOFF2 from the system, when one is around to try.
    #[test]
    fn decodes_a_real_font_when_present() {
        let path = std::env::var("SEPIA_WOFF2").unwrap_or_default();
        let Ok(bytes) = std::fs::read(&path) else { return };
        let sfnt = to_sfnt(&bytes).expect("decodes");
        let font = fontdue::Font::from_bytes(sfnt, fontdue::FontSettings::default()).expect("parses");
        // subsets may lack ASCII: take the first character the font maps
        let c = font.chars().keys().copied().find(|c| c.is_alphabetic()).expect("maps letters");
        let (m, _) = font.rasterize(c, 32.0);
        assert!(m.width > 5 && m.height > 5, "{m:?}");
    }
}
