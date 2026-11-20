//! Font fallback and colour emoji.
//!
//! Local files only: faces come from fixed system paths (or `SEPIA_FONT_FALLBACK`),
//! nothing is fetched and nothing is written. Fallback faces and the emoji face
//! load lazily, on the first codepoint the primary face cannot draw, so a page
//! of plain Latin text pays nothing.
//!
//! Colour emoji are bitmap fonts that fontdue cannot draw: Apple Color Emoji
//! keeps PNGs in `sbix`, Noto Color Emoji in `CBLC`/`CBDT`. Both are read here
//! with seeks into the file (Apple's is ~190 MB, so it is never read whole),
//! decoded with the `png` crate and scaled to the text size (box halving, then
//! bilinear interpolation).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use fontdue::{Font, FontSettings};

use crate::layout::Fonts;

// ---------------------------------------------------------------------------
// Fallback chain
// ---------------------------------------------------------------------------

/// Fallback faces, tried in order for a codepoint the primary face lacks.
/// Paths that do not exist on this machine are dropped at start-up.
const FALLBACK_FACES: &[(&str, u32)] = &[
    // macOS, in CoreText's cascade order (what Chrome draws there): the
    // system face first, its symbol companions, CJK, Arial Unicode last
    ("/System/Library/Fonts/SFNS.ttf", 0),
    ("/System/Library/Fonts/Apple Symbols.ttf", 0),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
    ("/System/Library/Fonts/CJKSymbolsFallback.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    // Linux
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/TTF/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
    // Windows
    (r"C:\Windows\Fonts\seguisym.ttf", 0),
    (r"C:\Windows\Fonts\msyh.ttc", 0),
    (r"C:\Windows\Fonts\malgun.ttf", 0),
];

/// Colour emoji faces, first existing one wins.
const EMOJI_FACES: &[&str] = &[
    "/System/Library/Fonts/Apple Color Emoji.ttc",
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
];

struct LazyFace {
    path: PathBuf,
    index: u32,
    face: OnceLock<Option<Font>>,
}

impl LazyFace {
    fn get(&self) -> Option<&Font> {
        self.face
            .get_or_init(|| {
                let bytes = std::fs::read(&self.path).ok()?;
                let settings = FontSettings { collection_index: self.index, ..FontSettings::default() };
                Font::from_bytes(bytes, settings).ok()
            })
            .as_ref()
    }
}

pub struct Fallbacks {
    faces: Vec<LazyFace>,
    /// codepoint -> index of the first face that has it (None: no face has it).
    hits: Mutex<HashMap<char, Option<u16>>>,
}

impl Fallbacks {
    pub fn system() -> Fallbacks {
        let mut list: Vec<(PathBuf, u32)> = std::env::var("SEPIA_FONT_FALLBACK")
            .ok()
            .map(|value| value.split(':').filter(|s| !s.is_empty()).map(|s| (PathBuf::from(s), 0)).collect())
            .unwrap_or_default();
        list.extend(FALLBACK_FACES.iter().map(|(p, i)| (PathBuf::from(p), *i)));
        Fallbacks::from_paths(list)
    }

    fn from_paths(list: Vec<(PathBuf, u32)>) -> Fallbacks {
        let faces = list
            .into_iter()
            .filter(|(path, _)| path.exists())
            .map(|(path, index)| LazyFace { path, index, face: OnceLock::new() })
            .collect();
        Fallbacks { faces, hits: Mutex::new(HashMap::new()) }
    }

    /// The first fallback face that has a glyph for `ch`.
    pub fn find(&self, ch: char) -> Option<&Font> {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let slot = match hits.get(&ch) {
            Some(slot) => *slot,
            None => {
                let found = self
                    .faces
                    .iter()
                    .position(|face| face.get().is_some_and(|font| font.has_glyph(ch)))
                    .map(|i| i as u16);
                hits.insert(ch, found);
                found
            }
        };
        drop(hits);
        slot.and_then(|i| self.faces[i as usize].get())
    }
}

pub struct LazyEmoji {
    paths: Vec<PathBuf>,
    face: OnceLock<Option<ColorEmoji>>,
}

impl LazyEmoji {
    pub fn system() -> LazyEmoji {
        let mut paths: Vec<PathBuf> = std::env::var("SEPIA_EMOJI_FONT")
            .ok()
            .map(|value| vec![PathBuf::from(value)])
            .unwrap_or_default();
        paths.extend(EMOJI_FACES.iter().map(PathBuf::from));
        paths.retain(|p| p.exists());
        LazyEmoji { paths, face: OnceLock::new() }
    }

    pub fn get(&self) -> Option<&ColorEmoji> {
        self.face
            .get_or_init(|| self.paths.iter().find_map(|p| ColorEmoji::open(p)))
            .as_ref()
    }
}

// ---------------------------------------------------------------------------
// Picking a face per codepoint
// ---------------------------------------------------------------------------

/// One drawable unit of a text run.
pub enum Glyph<'f> {
    Outline(&'f Font, char),
    Emoji(&'f ColorEmoji, u16),
}

/// Variable faces with an optical-size axis (San Francisco): browsers set
/// `opsz` to the font size, which changes the advances. Our rasteriser draws
/// the default instance, but the widths (and so line breaks) follow opsz.
static OPSZ_FACES: Mutex<Vec<(usize, &'static [u8])>> = Mutex::new(Vec::new());

thread_local! {
    static OPSZ_CACHE: std::cell::RefCell<HashMap<(usize, char, u32, u16), f32>> = std::cell::RefCell::new(HashMap::new());
    /// the weight the text being walked asks for (set by for_each_glyph_in):
    /// variable faces draw and measure at it
    static WEIGHT: std::cell::Cell<u16> = const { std::cell::Cell::new(400) };
}

/// The variations a variable face is drawn with at `size`: `opsz` = size,
/// `wght` = the text's weight (400 or 700), each clamped to the face's axis.
fn variations(bytes: &[u8], size: f32) -> Vec<(ttf_parser::Tag, f32)> {
    let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { return Vec::new() };
    let weight = WEIGHT.with(|w| w.get()) as f32;
    face.variation_axes().into_iter().filter_map(|a| {
        let want = if a.tag == ttf_parser::Tag::from_bytes(b"opsz") {
            size
        } else if a.tag == ttf_parser::Tag::from_bytes(b"wght") {
            weight
        } else {
            return None;
        };
        Some((a.tag, want.clamp(a.min_value, a.max_value)))
    }).collect()
}

/// A web font with `wght` / `opsz` axes is drawn (by swash) and measured at
/// the text's weight and size, not at its default instance.
pub fn register_variable(font: &Font, bytes: &[u8]) {
    register_opsz_or_wght(font, bytes);
}

/// Remember `bytes` as the file of `font` if it has an `opsz` axis.
fn register_opsz(font: &Font, bytes: &[u8]) {
    register_opsz_or_wght(font, bytes);
}

fn register_opsz_or_wght(font: &Font, bytes: &[u8]) {
    let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { return };
    if !face.variation_axes().into_iter().any(|a| a.tag == ttf_parser::Tag::from_bytes(b"opsz") || a.tag == ttf_parser::Tag::from_bytes(b"wght")) {
        return;
    }
    let leaked: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
    if let Ok(mut faces) = OPSZ_FACES.lock() {
        faces.push((font.file_hash(), leaked));
    }
}

fn opsz_advance(font: &Font, ch: char, size: f32) -> Option<f32> {
    let key = font.file_hash();
    let bytes = OPSZ_FACES.lock().ok()?.iter().find(|(k, _)| *k == key).map(|(_, b)| *b)?;
    let cache_key = (key, ch, size.to_bits(), WEIGHT.with(|w| w.get()));
    if let Some(v) = OPSZ_CACHE.with(|c| c.borrow().get(&cache_key).copied()) {
        return Some(v);
    }
    let mut face = ttf_parser::Face::parse(bytes, 0).ok()?;
    for (tag, value) in variations(bytes, size) {
        face.set_variation(tag, value);
    }
    let gid = face.glyph_index(ch)?;
    let advance = face.glyph_hor_advance(gid)? as f32 * size / face.units_per_em() as f32;
    OPSZ_CACHE.with(|c| c.borrow_mut().insert(cache_key, advance));
    Some(advance)
}

thread_local! {
    static SWASH: std::cell::RefCell<swash::scale::ScaleContext> = std::cell::RefCell::new(swash::scale::ScaleContext::new());
    static SWASH_CACHE: std::cell::RefCell<HashMap<(usize, char, u32, u16), Option<(i32, i32, usize, usize, std::rc::Rc<Vec<u8>>)>>> = std::cell::RefCell::new(HashMap::new());
}

/// A glyph of a variable face (one fontdue cannot draw), rendered by swash at
/// opsz = size: (xmin, ymin, width, height, coverage) in fontdue's convention
/// (ymin: the bitmap's bottom relative to the baseline, up positive).
pub fn variable_glyph(font: &Font, ch: char, size: f32) -> Option<(i32, i32, usize, usize, std::rc::Rc<Vec<u8>>)> {
    let key = font.file_hash();
    let bytes = OPSZ_FACES.lock().ok()?.iter().find(|(k, _)| *k == key).map(|(_, b)| *b)?;
    let cache_key = (key, ch, size.to_bits(), WEIGHT.with(|w| w.get()));
    if let Some(hit) = SWASH_CACHE.with(|c| c.borrow().get(&cache_key).cloned()) {
        return hit;
    }
    let rendered = (|| {
        let face = swash::FontRef::from_index(bytes, 0)?;
        let gid = face.charmap().map(ch);
        if gid == 0 {
            return None;
        }
        SWASH.with(|ctx| {
            let mut ctx = ctx.borrow_mut();
            let vars: Vec<swash::Setting<f32>> = variations(bytes, size).into_iter().map(|(tag, v)| swash::Setting { tag: u32::from_be_bytes(tag.to_bytes()), value: v }).collect();
            let mut scaler = ctx.builder(face).size(size).variations(vars.into_iter()).hint(false).build();
            let image = swash::scale::Render::new(&[swash::scale::Source::Outline])
                .format(swash::zeno::Format::Alpha)
                .render(&mut scaler, gid)?;
            let p = image.placement;
            Some((p.left, p.top - p.height as i32, p.width as usize, p.height as usize, std::rc::Rc::new(image.data)))
        })
    })();
    SWASH_CACHE.with(|c| c.borrow_mut().insert(cache_key, rendered.clone()));
    rendered
}

impl<'f> Glyph<'f> {
    pub fn advance(&self, size: f32) -> f32 {
        match self {
            Glyph::Outline(font, ch) => opsz_advance(font, *ch, size).unwrap_or_else(|| font.metrics(*ch, size).advance_width),
            Glyph::Emoji(face, gid) => face.advance(*gid, size),
        }
    }
}

// ---------------------------------------------------------------------------
// Families: fonts pages bring (@font-face) and local generic faces
// ---------------------------------------------------------------------------

/// Faces of one family: regular, bold, italic, bold italic. Each may come
/// in several files (unicode-range subsets: latin, latin-ext, cyrillic…);
/// the one with basic Latin goes first.
type Faces = [Vec<&'static Font>; 4];

fn slot(bold: bool, italic: bool) -> usize {
    (bold as usize) | ((italic as usize) << 1)
}

/// Fonts loaded from pages, by family name. A face lives for the rest of
/// the run (they are few and small); the same file is never parsed twice.
#[derive(Default)]
pub struct WebFonts {
    families: RwLock<HashMap<String, Faces>>,
}

static LOADED: OnceLock<Mutex<HashMap<String, Option<&'static Font>>>> = OnceLock::new();

impl WebFonts {
    pub fn register(&self, family: &str, bold: bool, italic: bool, font: &'static Font) {
        let mut map = self.families.write().unwrap_or_else(|e| e.into_inner());
        let files = &mut map.entry(family.to_ascii_lowercase()).or_default()[slot(bold, italic)];
        if files.iter().any(|f| std::ptr::eq(*f, font)) {
            return;
        }
        files.push(font);
        // the subset covering basic Latin leads; the others fill in what it lacks
        files.sort_by_key(|f| std::cmp::Reverse("aeiouxyz".chars().filter(|c| f.has_glyph(*c)).count()));
    }

    #[allow(dead_code)]
    pub fn has(&self, family: &str) -> bool {
        self.families.read().unwrap_or_else(|e| e.into_inner()).contains_key(family)
    }

    /// The face closest to bold/italic in `family`.
    fn face(&self, family: &str, bold: bool, italic: bool) -> Option<&'static Font> {
        let map = self.families.read().unwrap_or_else(|e| e.into_inner());
        let faces = map.get(family)?;
        Self::files(faces, bold, italic).first().copied()
    }

    fn files(faces: &Faces, bold: bool, italic: bool) -> &[&'static Font] {
        [slot(bold, italic), slot(false, italic), slot(bold, false), 0, 1, 2, 3].into_iter().map(|i| &faces[i][..]).find(|v| !v.is_empty()).unwrap_or(&[])
    }

    /// Another file of the same family and face that has `ch`.
    fn subset_with(&self, family: &str, bold: bool, italic: bool, ch: char) -> Option<&'static Font> {
        let map = self.families.read().unwrap_or_else(|e| e.into_inner());
        Self::files(map.get(family)?, bold, italic).iter().find(|f| f.has_glyph(ch)).copied()
    }
}

/// Parse a font file (TTF, OTF, TTC, WOFF or WOFF2) once per `key` (its URL).
pub fn load_web_font(key: &str, bytes: &[u8]) -> Option<&'static Font> {
    let cache = LOADED.get_or_init(Default::default);
    if let Some(done) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
        return *done;
    }
    let sfnt = if bytes.starts_with(b"wOFF") {
        woff_to_sfnt(bytes)
    } else if bytes.starts_with(b"wOF2") {
        crate::woff2::to_sfnt(bytes)
    } else {
        Some(bytes.to_vec())
    };
    let font = sfnt
        .filter(|b| b.starts_with(&[0, 1, 0, 0]) || b.starts_with(b"OTTO") || b.starts_with(b"true") || b.starts_with(b"ttcf"))
        .and_then(|b| {
            let font = Font::from_bytes(b.as_slice(), FontSettings::default()).ok()?;
            register_variable(&font, &b);
            Some(font)
        })
        .map(|f| &*Box::leak(Box::new(f)));
    if font.is_none() {
        let kind = if bytes.starts_with(b"wOFF") { "woff" } else if bytes.starts_with(b"wOF2") { "woff2" } else if bytes.starts_with(b"OTTO") { "otf (cff)" } else if bytes.first() == Some(&b'<') { "markup instead of a font" } else { "ttf or other" };
        crate::report::miss("font", kind);
    }
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), font);
    font
}

/// WOFF 1.0 → the sfnt it wraps: each table zlib-inflated back in place.
fn woff_to_sfnt(w: &[u8]) -> Option<Vec<u8>> {
    let flavor = be32(w, 4)?;
    let count = be16(w, 12)? as usize;
    let mut tables = Vec::with_capacity(count);
    for i in 0..count {
        let e = 44 + i * 20;
        let tag = w.get(e..e + 4)?.to_vec();
        let (offset, comp, orig, checksum) = (be32(w, e + 4)? as usize, be32(w, e + 8)? as usize, be32(w, e + 12)? as usize, be32(w, e + 16)?);
        let raw = w.get(offset..offset + comp)?;
        let data = if comp < orig { miniz_oxide::inflate::decompress_to_vec_zlib(raw).ok()? } else { raw.to_vec() };
        if data.len() != orig {
            return None;
        }
        tables.push((tag, checksum, data));
    }
    let mut out = Vec::new();
    let power = (count.max(1) as f32).log2().floor() as u32;
    let range = 16u16 << power;
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&(count as u16).to_be_bytes());
    out.extend_from_slice(&range.to_be_bytes());
    out.extend_from_slice(&(power as u16).to_be_bytes());
    out.extend_from_slice(&((count as u16) * 16 - range).to_be_bytes());
    let mut offset = 12 + count * 16;
    for (tag, checksum, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        offset += (data.len() + 3) & !3;
    }
    for (_, _, data) in &tables {
        out.extend_from_slice(data);
        out.resize((out.len() + 3) & !3, 0);
    }
    Some(out)
}

/// Local faces for the generic families pages ask for.
const MONO_FACES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/SFNSMono.ttf", 0),
    ("/System/Library/Fonts/Menlo.ttc", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0),
    (r"C:\Windows\Fonts\consola.ttf", 0),
];
/// The system UI face (`system-ui`, `-apple-system`): San Francisco on macOS.
const SYSTEM_FACES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/SFNS.ttf", 0),
    (r"C:\Windows\Fonts\segoeui.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
];
const SERIF_FACES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/Supplemental/Georgia.ttf", 0),
    ("/System/Library/Fonts/Supplemental/Times New Roman.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf", 0),
    (r"C:\Windows\Fonts\georgia.ttf", 0),
];

/// A local face loaded on first use.
#[derive(Default)]
pub struct LocalFamily {
    paths: &'static [(&'static str, u32)],
    face: OnceLock<Option<Font>>,
}

impl LocalFamily {
    pub fn mono() -> LocalFamily {
        LocalFamily { paths: MONO_FACES, face: OnceLock::new() }
    }

    pub fn serif() -> LocalFamily {
        LocalFamily { paths: SERIF_FACES, face: OnceLock::new() }
    }

    pub fn system() -> LocalFamily {
        LocalFamily { paths: SYSTEM_FACES, face: OnceLock::new() }
    }

    fn get(&self) -> Option<&Font> {
        self.face
            .get_or_init(|| {
                self.paths.iter().find_map(|(path, index)| {
                    let bytes = std::fs::read(path).ok()?;
                    let font = Font::from_bytes(bytes.as_slice(), FontSettings { collection_index: *index, ..FontSettings::default() }).ok()?;
                    if *index == 0 {
                        register_opsz(&font, &bytes);
                    }
                    Some(font)
                })
            })
            .as_ref()
    }
}

impl Fonts {
    /// The face for a CSS `font-family` list: the first family the page
    /// loaded, or a local face for a generic one; else the default faces.
    pub fn pick_family(&self, family: Option<&str>, bold: bool, italic: bool) -> &Font {
        if let Some(list) = family {
            for name in list.split(',') {
                let name = name.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
                if let Some(font) = self.web.face(&name, bold, italic) {
                    return font;
                }
                let local = match name.as_str() {
                    "monospace" | "ui-monospace" | "sfmono-regular" | "sf mono" | "menlo" | "monaco" | "consolas" | "courier" | "courier new" | "liberation mono" | "dejavu sans mono" => Some(&self.mono),
                    "serif" | "ui-serif" | "georgia" | "times" | "times new roman" | "cambria" => Some(&self.serif),
                    // San Francisco: a variable face, drawn by swash at its optical size.
                    // Off by default until its vertical metrics and macOS-like stroke
                    // weight match Chrome (example.com: 66 with it, 73 without); SEPIA_SF=1
                    "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-sans-serif" if !bold && !italic && sf_enabled() => Some(&self.system),
                    "sans-serif" | "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-sans-serif" | "arial" | "helvetica" | "helvetica neue" | "segoe ui" | "roboto" => return self.pick(bold, italic),
                    _ => None,
                };
                if let Some(font) = local.and_then(|l| l.get()) {
                    return font;
                }
            }
        }
        self.pick(bold, italic)
    }

    /// A page font file of the chosen family (another unicode-range subset) that has `ch`.
    fn web_subset(&self, family: Option<&str>, bold: bool, italic: bool, ch: char) -> Option<&'static Font> {
        let list = family?;
        for name in list.split(',') {
            let name = name.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
            if self.web.face(&name, bold, italic).is_some() {
                return self.web.subset_with(&name, bold, italic, ch);
            }
        }
        None
    }

    /// Advance of `text` in the face `family` selects.
    pub fn advance_in(&self, text: &str, size: f32, bold: bool, italic: bool, family: Option<&str>) -> f32 {
        let mut width = 0.0;
        self.for_each_glyph_in(family, text, bold, italic, |glyph| width += glyph.advance(size));
        width
    }

    pub fn for_each_glyph<'f>(&'f self, text: &str, bold: bool, italic: bool, f: impl FnMut(Glyph<'f>)) {
        self.for_each_glyph_in(None, text, bold, italic, f)
    }

    /// Walk `text`, choosing a face per codepoint: the styled primary face,
    /// then the fallback chain, then colour emoji. Emoji presentation wins
    /// for default-emoji codepoints and anything followed by U+FE0F.
    /// Joiners and variation selectors are zero-width and produce nothing.
    pub fn for_each_glyph_in<'f>(&'f self, family: Option<&str>, text: &str, bold: bool, italic: bool, mut f: impl FnMut(Glyph<'f>)) {
        WEIGHT.with(|w| w.set(if bold { 700 } else { 400 }));
        let primary = self.pick_family(family, bold, italic);
        let mut chars = text.chars().peekable();
        let mut after_emoji = false;
        while let Some(ch) = chars.next() {
            let cp = ch as u32;
            if is_zero_width(cp) {
                continue;
            }
            if after_emoji && (0x1F3FB..=0x1F3FF).contains(&cp) {
                // Skin-tone modifier: without shaping the base glyph stays,
                // the swatch would only add noise.
                continue;
            }
            if (0x1F1E6..=0x1F1FF).contains(&cp) {
                // Regional indicators only become flags through ligature
                // shaping; until there is shaping, show the letters (🇺🇦 -> UA)
                // instead of nothing.
                let letter = char::from_u32('A' as u32 + (cp - 0x1F1E6)).unwrap_or('?');
                f(Glyph::Outline(primary, letter));
                after_emoji = false;
                continue;
            }
            let next = chars.peek().map(|c| *c as u32);
            let text_style = next == Some(0xFE0E);
            let emoji_style = next == Some(0xFE0F) || is_default_emoji(cp);
            if emoji_style && !text_style {
                if let Some(face) = self.emoji.get() {
                    if let Some(gid) = face.glyph(ch) {
                        f(Glyph::Emoji(face, gid));
                        after_emoji = true;
                        continue;
                    }
                }
            }
            after_emoji = false;
            if ch.is_control() || primary.has_glyph(ch) {
                f(Glyph::Outline(primary, ch));
            } else if let Some(font) = self.web_subset(family, bold, italic, ch) {
                f(Glyph::Outline(font, ch));
            } else if let Some(font) = self.fallback.find(ch) {
                f(Glyph::Outline(font, ch));
            } else if let Some((face, gid)) = self.emoji.get().and_then(|e| e.glyph(ch).map(|g| (e, g))) {
                f(Glyph::Emoji(face, gid));
                after_emoji = true;
            } else {
                // Nothing local can draw it: the primary face's .notdef box.
                f(Glyph::Outline(primary, ch));
            }
        }
    }
}

fn is_zero_width(cp: u32) -> bool {
    matches!(cp, 0x200D | 0xFE0E | 0xFE0F | 0x20E3) || (0xE0020..=0xE007F).contains(&cp)
}

/// Codepoints with Emoji_Presentation=Yes (Unicode emoji-data), condensed.
fn is_default_emoji(cp: u32) -> bool {
    const BMP: &[(u32, u32)] = &[
        (0x231A, 0x231B), (0x23E9, 0x23EC), (0x23F0, 0x23F0), (0x23F3, 0x23F3),
        (0x25FD, 0x25FE), (0x2614, 0x2615), (0x2648, 0x2653), (0x267F, 0x267F),
        (0x2693, 0x2693), (0x26A1, 0x26A1), (0x26AA, 0x26AB), (0x26BD, 0x26BE),
        (0x26C4, 0x26C5), (0x26CE, 0x26CE), (0x26D4, 0x26D4), (0x26EA, 0x26EA),
        (0x26F2, 0x26F3), (0x26F5, 0x26F5), (0x26FA, 0x26FA), (0x26FD, 0x26FD),
        (0x2705, 0x2705), (0x270A, 0x270B), (0x2728, 0x2728), (0x274C, 0x274C),
        (0x274E, 0x274E), (0x2753, 0x2755), (0x2757, 0x2757), (0x2795, 0x2797),
        (0x27B0, 0x27B0), (0x27BF, 0x27BF), (0x2B1B, 0x2B1C), (0x2B50, 0x2B50),
        (0x2B55, 0x2B55),
    ];
    if (0x1F000..=0x1FAFF).contains(&cp) {
        // Enclosed alphanumerics (🄰, ①…) default to text; regional indicators do not.
        return !(0x1F100..=0x1F1E5).contains(&cp);
    }
    BMP.iter().any(|&(lo, hi)| (lo..=hi).contains(&cp))
}

// ---------------------------------------------------------------------------
// Colour emoji face (sbix / CBDT)
// ---------------------------------------------------------------------------

trait Source: Read + Seek + Send {}
impl<T: Read + Seek + Send> Source for T {}

/// A decoded, scaled glyph bitmap. `left`/`top` are the offset of the
/// bitmap's top-left corner from the pen position on the baseline (y down).
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub left: f32,
    pub top: f32,
    /// Premultiplied RGBA, row-major.
    pub rgba: Vec<u8>,
}

enum Strikes {
    Sbix(Vec<SbixStrike>),
    Cbdt { cblc: Vec<u8>, cbdt: u64, sizes: Vec<CbSize> },
}

struct SbixStrike {
    ppem: u16,
    offset: u64,
}

struct CbSize {
    ppem: u8,
    /// (first gid, last gid, index subtable offset inside CBLC)
    subtables: Vec<(u16, u16, usize)>,
}

pub struct ColorEmoji {
    src: Mutex<Box<dyn Source>>,
    cmap: HashMap<u32, u16>,
    units_per_em: f32,
    advances: Vec<u16>,
    strikes: Strikes,
    cache: Mutex<HashMap<(u16, u32), Option<Arc<Bitmap>>>>,
}

impl ColorEmoji {
    pub fn open(path: &Path) -> Option<ColorEmoji> {
        let file = File::open(path).ok()?;
        ColorEmoji::from_source(Box::new(BufReader::new(file)))
    }

    fn from_source(mut src: Box<dyn Source>) -> Option<ColorEmoji> {
        let header = read_at(&mut src, 0, 16)?;
        let font_offset = if &header[0..4] == b"ttcf" { be32(&header, 12)? as u64 } else { 0 };
        let dir = read_at(&mut src, font_offset, 12)?;
        let num_tables = be16(&dir, 4)? as usize;
        let records = read_at(&mut src, font_offset + 12, num_tables * 16)?;
        let mut tables: HashMap<[u8; 4], (u64, usize)> = HashMap::new();
        for i in 0..num_tables {
            let r = &records[i * 16..i * 16 + 16];
            let tag = [r[0], r[1], r[2], r[3]];
            tables.insert(tag, (be32(r, 8)? as u64, be32(r, 12)? as usize));
        }
        let table = |src: &mut Box<dyn Source>, tag: &[u8; 4]| -> Option<Vec<u8>> {
            let (offset, len) = *tables.get(tag)?;
            read_at(src, offset, len)
        };

        let head = table(&mut src, b"head")?;
        let units_per_em = be16(&head, 18)? as f32;
        let hhea = table(&mut src, b"hhea")?;
        let metrics_count = be16(&hhea, 34)? as usize;
        let hmtx = table(&mut src, b"hmtx")?;
        let advances: Vec<u16> =
            (0..metrics_count).filter_map(|i| be16(&hmtx, i * 4)).collect();
        let cmap = parse_cmap(&table(&mut src, b"cmap")?)?;

        let strikes = if let Some(&(offset, _)) = tables.get(b"sbix") {
            let head = read_at(&mut src, offset, 8)?;
            let count = be32(&head, 4)? as usize;
            let offsets = read_at(&mut src, offset + 8, count * 4)?;
            let mut list = Vec::with_capacity(count);
            for i in 0..count {
                let strike = offset + be32(&offsets, i * 4)? as u64;
                let ppem = be16(&read_at(&mut src, strike, 2)?, 0)?;
                list.push(SbixStrike { ppem, offset: strike });
            }
            list.sort_by_key(|s| s.ppem);
            Strikes::Sbix(list)
        } else if tables.contains_key(b"CBLC") && tables.contains_key(b"CBDT") {
            let cblc = table(&mut src, b"CBLC")?;
            let cbdt = tables[b"CBDT"].0;
            let count = be32(&cblc, 4)? as usize;
            let mut sizes = Vec::with_capacity(count);
            for i in 0..count {
                let rec = 8 + i * 48;
                let array = be32(&cblc, rec)? as usize;
                let subtables = be32(&cblc, rec + 8)? as usize;
                let ppem = *cblc.get(rec + 44)?;
                let mut list = Vec::with_capacity(subtables);
                for j in 0..subtables {
                    let entry = array + j * 8;
                    list.push((be16(&cblc, entry)?, be16(&cblc, entry + 2)?, array + be32(&cblc, entry + 4)? as usize));
                }
                sizes.push(CbSize { ppem, subtables: list });
            }
            sizes.sort_by_key(|s| s.ppem);
            Strikes::Cbdt { cblc, cbdt, sizes }
        } else {
            return None;
        };

        Some(ColorEmoji {
            src: Mutex::new(src),
            cmap,
            units_per_em: units_per_em.max(1.0),
            advances,
            strikes,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn glyph(&self, ch: char) -> Option<u16> {
        self.cmap.get(&(ch as u32)).copied().filter(|g| *g != 0)
    }

    pub fn advance(&self, gid: u16, size: f32) -> f32 {
        let units = self
            .advances
            .get(gid as usize)
            .or(self.advances.last())
            .copied()
            .unwrap_or(self.units_per_em as u16);
        units as f32 * size / self.units_per_em
    }

    /// The glyph as a bitmap for text of `size` px, cached per (glyph, px).
    pub fn bitmap(&self, gid: u16, size: f32) -> Option<Arc<Bitmap>> {
        let px = size.round().max(1.0) as u32;
        if let Some(hit) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(&(gid, px)) {
            return hit.clone();
        }
        let made = self.render(gid, px as f32, 0).map(Arc::new);
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert((gid, px), made.clone());
        made
    }

    fn render(&self, gid: u16, px: f32, depth: u8) -> Option<Bitmap> {
        let mut src = self.src.lock().unwrap_or_else(|e| e.into_inner());
        match &self.strikes {
            Strikes::Sbix(strikes) => {
                let strike = strikes.iter().find(|s| s.ppem as f32 >= px).or(strikes.last())?;
                let offsets = read_at(&mut src, strike.offset + 4 + gid as u64 * 4, 8)?;
                let (start, end) = (be32(&offsets, 0)? as u64, be32(&offsets, 4)? as u64);
                if end <= start + 8 {
                    return None;
                }
                let data = read_at(&mut src, strike.offset + start, (end - start) as usize)?;
                drop(src);
                let origin_x = be16(&data, 0)? as i16 as f32;
                let origin_y = be16(&data, 2)? as i16 as f32;
                match &data[4..8] {
                    b"png " => {
                        let (w, h, rgba) = decode_png(&data[8..])?;
                        let scale = px / strike.ppem.max(1) as f32;
                        Some(place(w, h, &rgba, scale, origin_x * scale, -(origin_y + h as f32) * scale))
                    }
                    b"dupe" if depth == 0 => self.render(be16(&data, 8)?, px, 1),
                    _ => None,
                }
            }
            Strikes::Cbdt { cblc, cbdt, sizes } => {
                let size = sizes.iter().find(|s| s.ppem as f32 >= px).or(sizes.last())?;
                let &(first, _, sub) = size.subtables.iter().find(|(lo, hi, _)| (*lo..=*hi).contains(&gid))?;
                let index_format = be16(cblc, sub)?;
                let image_format = be16(cblc, sub + 2)?;
                let image_base = *cbdt + be32(cblc, sub + 4)? as u64;
                let n = (gid - first) as usize;
                let mut shared_metrics: Option<[u8; 8]> = None;
                let (start, len) = match index_format {
                    1 => {
                        let a = be32(cblc, sub + 8 + n * 4)? as u64;
                        (a, (be32(cblc, sub + 12 + n * 4)? as u64).checked_sub(a)? as usize)
                    }
                    2 => {
                        let image_size = be32(cblc, sub + 8)? as usize;
                        shared_metrics = Some(cblc.get(sub + 12..sub + 20)?.try_into().ok()?);
                        ((n * image_size) as u64, image_size)
                    }
                    3 => {
                        let a = be16(cblc, sub + 8 + n * 2)? as u64;
                        (a, (be16(cblc, sub + 10 + n * 2)? as u64).checked_sub(a)? as usize)
                    }
                    _ => return None,
                };
                if len == 0 {
                    return None;
                }
                let data = read_at(&mut src, image_base + start, len)?;
                drop(src);
                // (bearing x, bearing y, png bytes)
                let (bx, by, png) = match image_format {
                    17 => (data[2] as i8, data[3] as i8, data.get(9..9 + be32(&data, 5)? as usize)?),
                    18 => (data[2] as i8, data[3] as i8, data.get(12..12 + be32(&data, 8)? as usize)?),
                    19 => {
                        let m = shared_metrics?;
                        (m[2] as i8, m[3] as i8, data.get(4..4 + be32(&data, 0)? as usize)?)
                    }
                    _ => return None,
                };
                let (w, h, rgba) = decode_png(png)?;
                let scale = px / size.ppem.max(1) as f32;
                Some(place(w, h, &rgba, scale, bx as f32 * scale, -(by as f32) * scale))
            }
        }
    }
}

fn place(w: usize, h: usize, rgba: &[u8], scale: f32, left: f32, top: f32) -> Bitmap {
    let dw = ((w as f32 * scale).round() as usize).max(1);
    let dh = ((h as f32 * scale).round() as usize).max(1);
    Bitmap { width: dw, height: dh, left, top, rgba: resample(rgba, w, h, dw, dh) }
}

fn read_at(src: &mut Box<dyn Source>, offset: u64, len: usize) -> Option<Vec<u8>> {
    // A corrupt length must not turn into a huge allocation.
    if len > 64 * 1024 * 1024 {
        return None;
    }
    src.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    src.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Unicode cmap: format 12 (full range) preferred, format 4 (BMP) otherwise.
fn parse_cmap(cmap: &[u8]) -> Option<HashMap<u32, u16>> {
    let count = be16(cmap, 2)? as usize;
    let mut best: Option<(u16, usize)> = None;
    for i in 0..count {
        let rec = 4 + i * 8;
        let (platform, encoding) = (be16(cmap, rec)?, be16(cmap, rec + 2)?);
        let offset = be32(cmap, rec + 4)? as usize;
        let unicode = platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10));
        if !unicode {
            continue;
        }
        let format = be16(cmap, offset)?;
        if (format == 12 || format == 4) && best.map_or(true, |(f, _)| f != 12) {
            best = Some((format, offset));
        }
    }
    let (format, at) = best?;
    let mut map = HashMap::new();
    if format == 12 {
        let groups = be32(cmap, at + 12)? as usize;
        for g in 0..groups {
            let rec = at + 16 + g * 12;
            let (start, end, glyph) = (be32(cmap, rec)?, be32(cmap, rec + 4)?, be32(cmap, rec + 8)?);
            for cp in start..=end.min(start.saturating_add(0x10000)) {
                map.insert(cp, (glyph + (cp - start)) as u16);
            }
        }
    } else {
        let segs = be16(cmap, at + 6)? as usize / 2;
        let ends = at + 14;
        let starts = ends + segs * 2 + 2;
        let deltas = starts + segs * 2;
        let ranges = deltas + segs * 2;
        for s in 0..segs {
            let (end, start) = (be16(cmap, ends + s * 2)?, be16(cmap, starts + s * 2)?);
            let delta = be16(cmap, deltas + s * 2)?;
            let range = be16(cmap, ranges + s * 2)? as usize;
            if start == 0xFFFF {
                continue;
            }
            for cp in start..=end {
                let glyph = if range == 0 {
                    cp.wrapping_add(delta)
                } else {
                    let at = ranges + s * 2 + range + (cp - start) as usize * 2;
                    match be16(cmap, at)? {
                        0 => 0,
                        g => g.wrapping_add(delta),
                    }
                };
                if glyph != 0 {
                    map.insert(cp as u32, glyph);
                }
            }
        }
    }
    Some(map)
}

/// PNG -> (width, height, premultiplied RGBA8).
fn decode_png(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None, // EXPAND turns this into RGB(A)
    };
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &buf[y * info.line_size..];
        for x in 0..w {
            let p = &row[x * channels..x * channels + channels];
            let (r, g, b, a) = match channels {
                4 => (p[0], p[1], p[2], p[3]),
                3 => (p[0], p[1], p[2], 255),
                2 => (p[0], p[0], p[0], p[1]),
                _ => (p[0], p[0], p[0], 255),
            };
            let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
            out.extend_from_slice(&[pm(r), pm(g), pm(b), a]);
        }
    }
    Some((w, h, out))
}

/// Scale premultiplied RGBA: halve with a 2×2 box while the source is at
/// least twice the target (keeps detail when shrinking 160 px art to 16 px),
/// then bilinear interpolation to the exact size.
fn resample(src: &[u8], mut sw: usize, mut sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut cur: Vec<u8> = src.to_vec();
    while sw >= dw * 2 && sh >= dh * 2 && sw >= 2 && sh >= 2 {
        let (nw, nh) = (sw / 2, sh / 2);
        let mut next = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let at = |xx: usize, yy: usize| cur[(yy * sw + xx) * 4 + c] as u32;
                    let sum = at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1);
                    next[(y * nw + x) * 4 + c] = ((sum + 2) / 4) as u8;
                }
            }
        }
        cur = next;
        sw = nw;
        sh = nh;
    }
    if sw == dw && sh == dh {
        return cur;
    }
    let mut out = vec![0u8; dw * dh * 4];
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..dw {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (sx.floor() as usize, sx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            for c in 0..4 {
                let p = |xx: usize, yy: usize| cur[(yy * sw + xx) * 4 + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bottom = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                out[(y * dw + x) * 4 + c] = (top * (1.0 - ty) + bottom * ty).round() as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
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

    /// A minimal sfnt with head/hhea/hmtx/maxp/cmap(format 12: U+1F600 -> gid 1)
    /// plus the given colour tables. Table offsets are absolute, like a real file.
    fn build_font(extra: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
        let mut hmtx = Vec::new();
        for adv in [500u16, 1200] {
            hmtx.extend_from_slice(&adv.to_be_bytes());
            hmtx.extend_from_slice(&0u16.to_be_bytes());
        }
        let mut maxp = vec![0, 0, 0x50, 0, 0, 2];
        maxp.truncate(6);
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&[0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12]);
        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&28u32.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
        for v in [0x1F600u32, 0x1F600, 1] {
            cmap.extend_from_slice(&v.to_be_bytes());
        }
        let mut tables = vec![
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"maxp", maxp),
            (*b"cmap", cmap),
        ];
        tables.extend(extra);
        let mut out = vec![0, 1, 0, 0];
        out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0; 6]);
        let mut offset = 12 + tables.len() * 16;
        let mut body = Vec::new();
        for (tag, data) in &tables {
            out.extend_from_slice(tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            offset += data.len();
        }
        out.extend(body);
        out
    }

    fn open(bytes: Vec<u8>) -> ColorEmoji {
        ColorEmoji::from_source(Box::new(Cursor::new(bytes))).expect("parses")
    }

    #[test]
    fn reads_sbix_png_and_scales_it() {
        let png = png_bytes(40, 40, [255, 0, 0, 255]);
        let mut sbix = vec![0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 12];
        // strike at 12: ppem 40, ppi 72, offsets for 3 entries (2 glyphs + end)
        sbix.extend_from_slice(&40u16.to_be_bytes());
        sbix.extend_from_slice(&72u16.to_be_bytes());
        let data_start = 4 + 3 * 4;
        for v in [data_start, data_start, data_start + 8 + png.len()] {
            sbix.extend_from_slice(&(v as u32).to_be_bytes());
        }
        sbix.extend_from_slice(&0i16.to_be_bytes());
        sbix.extend_from_slice(&(-4i16).to_be_bytes());
        sbix.extend_from_slice(b"png ");
        sbix.extend_from_slice(&png);
        let face = open(build_font(vec![(*b"sbix", sbix)]));

        assert_eq!(face.glyph('\u{1F600}'), Some(1));
        assert_eq!(face.glyph('A'), None);
        assert!((face.advance(1, 10.0) - 12.0).abs() < 1e-4);
        let bmp = face.bitmap(1, 10.0).expect("bitmap");
        assert_eq!((bmp.width, bmp.height), (10, 10));
        // origin y -4 at ppem 40 -> bottom 1px below the baseline
        assert!((bmp.top - -9.0).abs() < 1e-4, "top {}", bmp.top);
        assert!(bmp.rgba.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
        assert!(face.bitmap(0, 10.0).is_none(), "empty glyph");
    }

    #[test]
    fn reads_cbdt_png() {
        let png = png_bytes(20, 20, [0, 0, 255, 128]);
        // CBDT: version, then glyph 1 in image format 17.
        let mut cbdt = vec![0, 3, 0, 0];
        let image_start = cbdt.len();
        cbdt.extend_from_slice(&[20, 20, 1, 18, 22]); // h, w, bearingX, bearingY, advance
        cbdt.extend_from_slice(&(png.len() as u32).to_be_bytes());
        cbdt.extend_from_slice(&png);
        let image_end = cbdt.len();

        let mut cblc = vec![0, 3, 0, 0, 0, 0, 0, 1];
        let array = 8 + 48;
        let mut size = vec![0u8; 48];
        size[0..4].copy_from_slice(&(array as u32).to_be_bytes());
        size[8..12].copy_from_slice(&1u32.to_be_bytes());
        size[40..42].copy_from_slice(&1u16.to_be_bytes());
        size[42..44].copy_from_slice(&1u16.to_be_bytes());
        size[44] = 20;
        size[45] = 20;
        cblc.extend(size);
        cblc.extend_from_slice(&1u16.to_be_bytes());
        cblc.extend_from_slice(&1u16.to_be_bytes());
        cblc.extend_from_slice(&8u32.to_be_bytes());
        // index subtable format 1, image format 17, data offset 0
        cblc.extend_from_slice(&1u16.to_be_bytes());
        cblc.extend_from_slice(&17u16.to_be_bytes());
        cblc.extend_from_slice(&0u32.to_be_bytes());
        cblc.extend_from_slice(&(image_start as u32).to_be_bytes());
        cblc.extend_from_slice(&(image_end as u32).to_be_bytes());

        let face = open(build_font(vec![(*b"CBLC", cblc), (*b"CBDT", cbdt)]));
        let bmp = face.bitmap(1, 20.0).expect("bitmap");
        assert_eq!((bmp.width, bmp.height), (20, 20));
        assert!((bmp.left - 1.0).abs() < 1e-4 && (bmp.top - -18.0).abs() < 1e-4);
        // premultiplied: 255 * 128 / 255
        assert_eq!(&bmp.rgba[0..4], &[0, 0, 128, 128]);
    }

    #[test]
    fn resample_keeps_flat_colour_and_size() {
        let src = [10u8, 20, 30, 255].repeat(160 * 160);
        let out = resample(&src, 160, 160, 17, 17);
        assert_eq!(out.len(), 17 * 17 * 4);
        assert!(out.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
        let up = resample(&[0, 0, 0, 255, 255, 255, 255, 255], 2, 1, 4, 1);
        assert_eq!(up.len(), 16);
        assert!(up[4] > 0 && up[4] < 255, "interpolated: {:?}", up);
    }

    #[test]
    fn emoji_classification() {
        assert!(is_default_emoji(0x1F600));
        assert!(is_default_emoji(0x2705));
        assert!(!is_default_emoji('A' as u32));
        assert!(!is_default_emoji(0x00A9), "© is text by default");
        assert!(!is_default_emoji(0x1F130), "enclosed letters are text");
        assert!(is_zero_width(0x200D) && is_zero_width(0xFE0F));
    }

    #[test]
    fn flags_fall_back_to_their_letters() {
        let fonts = Fonts::load();
        let flag = fonts.advance("\u{1F1FA}\u{1F1E6}", 16.0, false, false);
        let letters = fonts.advance("UA", 16.0, false, false);
        assert!((flag - letters).abs() < 1e-3, "flag {flag} vs letters {letters}");
    }

    #[test]
    fn joiners_and_selectors_take_no_space() {
        let fonts = Fonts::load();
        let plain = fonts.advance("ab", 16.0, false, false);
        let joined = fonts.advance("a\u{200D}\u{FE0F}b", 16.0, false, false);
        assert!((plain - joined).abs() < 1e-3);
    }

    #[test]
    fn digits_stay_text_but_emoji_go_colour() {
        let fonts = Fonts::load();
        let mut kinds = Vec::new();
        fonts.for_each_glyph("1\u{1F600}", false, false, |g| {
            kinds.push(matches!(g, Glyph::Emoji(..)))
        });
        assert!(!kinds[0], "a digit must not become the keycap emoji base");
        if fonts.emoji.get().is_some() {
            assert_eq!(kinds, vec![false, true]);
        }
    }

    #[test]
    fn missing_codepoints_use_fallback_faces() {
        let fonts = Fonts::load();
        let ch = '\u{4E2D}'; // 中
        if fonts.pick(false, false).has_glyph(ch) || fonts.fallback.find(ch).is_none() {
            return; // primary already covers CJK, or this machine has no CJK face
        }
        let mut faces = Vec::new();
        fonts.for_each_glyph("a\u{4E2D}", false, false, |g| {
            if let Glyph::Outline(font, _) = g {
                faces.push(font as *const Font)
            }
        });
        assert_eq!(faces.len(), 2);
        assert_ne!(faces[0], faces[1], "中 must come from a fallback face");
    }

    #[test]
    fn real_emoji_face_renders_when_installed() {
        let emoji = LazyEmoji::system();
        let Some(face) = emoji.get() else { return };
        let gid = face.glyph('\u{1F600}').expect("😀 in the system emoji font");
        let bmp = face.bitmap(gid, 32.0).expect("bitmap");
        assert!(bmp.width >= 24 && bmp.width <= 48, "width {}", bmp.width);
        let coloured = bmp
            .rgba
            .chunks_exact(4)
            .filter(|p| p[3] > 200 && (p[0] as i32 - p[2] as i32).abs() > 60)
            .count();
        assert!(coloured > 50, "expected a yellow face, got {coloured} coloured pixels");
    }
}

/// SEPIA_SF, read once (font picking runs for every text run).
fn sf_enabled() -> bool {
    static SF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SF.get_or_init(|| std::env::var_os("SEPIA_SF").is_some())
}
