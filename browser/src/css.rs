//! CSS: a real (small) parser, selector matching, and the cascade.

/// (sheet, node, a hash of the node's tag, attributes and ancestors, viewport):
/// a node built and dropped during layout may reuse an address, the hash
/// tells it apart without copying anything.
type MatchingKey = (usize, usize, u64, u32, u32);
type MatchingCache = std::collections::HashMap<MatchingKey, std::rc::Rc<Vec<Declaration>>, FastHash>;

/// FxHash: a fast non-cryptographic hasher for the engine's own tables
/// (keys are not attacker-chosen in a way that matters here).
#[derive(Default, Clone, Copy)]
pub struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(word)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (self.0.rotate_left(5) ^ n as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

pub type FastHash = std::hash::BuildHasherDefault<Fx>;

/// `var()` substitutions of one layout, by (stylesheet declaration, variable
/// scope); the scopes are kept alive here so their addresses stay theirs.
#[derive(Default)]
struct VarCache {
    results: std::collections::HashMap<(usize, usize), Option<std::rc::Rc<str>>, FastHash>,
    scopes: std::collections::HashMap<usize, std::sync::Arc<Vars>, FastHash>,
}

thread_local! {
    static LAYOUT_MATCHES: std::cell::RefCell<Option<MatchingCache>> = const { std::cell::RefCell::new(None) };
    static LAYOUT_VARS: std::cell::RefCell<Option<VarCache>> = const { std::cell::RefCell::new(None) };
}

/// A declaration's value with its `var()`s replaced: borrowed when it has
/// none; for stylesheet declarations (`shared`: alive for the whole layout)
/// remembered per variable scope, since most elements share their parent's.
pub fn substitute_in<'a>(value: &'a str, vars: &std::sync::Arc<Vars>, shared: bool) -> Option<std::borrow::Cow<'a, str>> {
    if !has_var(value) {
        return Some(std::borrow::Cow::Borrowed(value));
    }
    if shared {
        let key = (value.as_ptr() as usize ^ (value.len() << 48), std::sync::Arc::as_ptr(vars) as usize);
        let hit = LAYOUT_VARS.with(|c| c.borrow().as_ref().map(|c| c.results.get(&key).cloned()));
        match hit {
            Some(Some(found)) => return found.map(|v| std::borrow::Cow::Owned(v.to_string())),
            Some(None) => {
                let out = substitute_vars(value, &**vars);
                LAYOUT_VARS.with(|c| {
                    if let Some(c) = c.borrow_mut().as_mut() {
                        c.scopes.entry(key.1).or_insert_with(|| vars.clone());
                        c.results.insert(key, out.as_deref().map(std::rc::Rc::from));
                    }
                });
                return out.map(std::borrow::Cow::Owned);
            }
            None => {}
        }
    }
    substitute_vars(value, &**vars).map(std::borrow::Cow::Owned)
}

/// A document is immutable during one layout. Intrinsic-size probes revisit
/// the same elements, but selector matching need only run once per element.
/// The scope ends before DOM changes or another page can reuse its addresses.
pub(crate) struct MatchingScope(Option<MatchingCache>, Option<VarCache>);

pub(crate) fn cache_layout_matches() -> MatchingScope {
    MatchingScope(
        LAYOUT_MATCHES.with(|cache| cache.replace(Some(MatchingCache::default()))),
        LAYOUT_VARS.with(|cache| cache.replace(Some(VarCache::default()))),
    )
}

impl Drop for MatchingScope {
    fn drop(&mut self) {
        LAYOUT_MATCHES.with(|cache| cache.replace(self.0.take()));
        LAYOUT_VARS.with(|cache| cache.replace(self.1.take()));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLUE: Color = Color::rgb(0, 0, 238);
    pub const GRAY: Color = Color::rgb(128, 128, 128);
    #[allow(dead_code)]
    pub const LIGHT_GRAY: Color = Color::rgb(238, 238, 238);

    #[allow(dead_code)]
    pub fn with_alpha(self, a: u8) -> Color {
        Color { a, ..self }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }
}


// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

/// CSS colours: #rgb #rgba #rrggbb #rrggbbaa, rgb()/rgba()/hsl()/hsla() in
/// both the comma and the space/slash syntax, every named colour, transparent.
/// `currentcolor` is resolved by the caller.
pub fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let digit = |s: &str| u8::from_str_radix(s, 16).ok();
        let double = |c: &str| digit(&c.repeat(2));
        return match hex.len() {
            3 => Some(Color::rgb(double(&hex[0..1])?, double(&hex[1..2])?, double(&hex[2..3])?)),
            4 => Some(Color::rgba(double(&hex[0..1])?, double(&hex[1..2])?, double(&hex[2..3])?, double(&hex[3..4])?)),
            6 => Some(Color::rgb(digit(&hex[0..2])?, digit(&hex[2..4])?, digit(&hex[4..6])?)),
            8 => Some(Color::rgba(digit(&hex[0..2])?, digit(&hex[2..4])?, digit(&hex[4..6])?, digit(&hex[6..8])?)),
            _ => None,
        };
    }
    if let Some((func, rest)) = value.split_once('(') {
        let inner = rest.strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let alpha = match parts.get(3) {
            Some(a) => {
                let v = if let Some(p) = a.strip_suffix('%') { p.parse::<f32>().ok()? / 100.0 } else { a.parse::<f32>().ok()? };
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            }
            None => 255,
        };
        return match func.trim() {
            "rgb" | "rgba" => {
                let channel = |p: &str| -> Option<u8> {
                    let v = if let Some(pct) = p.strip_suffix('%') { pct.parse::<f32>().ok()? * 2.55 } else { p.parse::<f32>().ok()? };
                    Some(v.round().clamp(0.0, 255.0) as u8)
                };
                Some(Color::rgba(channel(parts[0])?, channel(parts[1])?, channel(parts[2])?, alpha))
            }
            "hsl" | "hsla" => {
                let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?.rem_euclid(360.0);
                let s = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
                let l = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
                let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
                let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
                let (r, g, b) = match (h / 60.0) as u32 {
                    0 => (c, x, 0.0),
                    1 => (x, c, 0.0),
                    2 => (0.0, c, x),
                    3 => (0.0, x, c),
                    4 => (x, 0.0, c),
                    _ => (c, 0.0, x),
                };
                let m = l - c / 2.0;
                let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
                Some(Color::rgba(to(r), to(g), to(b), alpha))
            }
            _ => None,
        };
    }
    if value == "transparent" {
        return Some(Color::rgba(0, 0, 0, 0));
    }
    NAMED.iter().find(|(name, _)| *name == value).map(|(_, rgb)| Color::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, *rgb as u8))
}

const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff), ("aquamarine", 0x7fffd4), ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc), ("bisque", 0xffe4c4), ("black", 0x000000), ("blanchedalmond", 0xffebcd), ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a), ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e), ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed), ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c),
    ("cyan", 0x00ffff), ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b), ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9), ("darkkhaki", 0xbdb76b), ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc), ("darkred", 0x8b0000), ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b), ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f), ("darkturquoise", 0x00ced1), ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493), ("deepskyblue", 0x00bfff), ("dimgray", 0x696969), ("dimgrey", 0x696969), ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22), ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff), ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080), ("green", 0x008000),
    ("greenyellow", 0xadff2f), ("grey", 0x808080), ("honeydew", 0xf0fff0), ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082), ("ivory", 0xfffff0), ("khaki", 0xf0e68c), ("lavender", 0xe6e6fa), ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00), ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6), ("lightcoral", 0xf08080), ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3), ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a), ("lightseagreen", 0x20b2aa), ("lightskyblue", 0x87cefa), ("lightslategray", 0x778899), ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de), ("lightyellow", 0xffffe0), ("lime", 0x00ff00), ("limegreen", 0x32cd32), ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff), ("maroon", 0x800000), ("mediumaquamarine", 0x66cdaa), ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee), ("mediumspringgreen", 0x00fa9a), ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585), ("midnightblue", 0x191970), ("mintcream", 0xf5fffa), ("mistyrose", 0xffe4e1), ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead), ("navy", 0x000080), ("oldlace", 0xfdf5e6), ("olive", 0x808000), ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6), ("palegoldenrod", 0xeee8aa), ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee), ("palevioletred", 0xdb7093), ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9), ("peru", 0xcd853f),
    ("pink", 0xffc0cb), ("plum", 0xdda0dd), ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399),
    ("red", 0xff0000), ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1), ("saddlebrown", 0x8b4513), ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460), ("seagreen", 0x2e8b57), ("seashell", 0xfff5ee), ("sienna", 0xa0522d), ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd), ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f), ("steelblue", 0x4682b4), ("tan", 0xd2b48c), ("teal", 0x008080), ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347), ("turquoise", 0x40e0d0), ("violet", 0xee82ee), ("wheat", 0xf5deb3), ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00), ("yellowgreen", 0x9acd32),
];

// ---------------------------------------------------------------------------
// Rules and selectors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp {
    Exists,
    Equals,
    /// ~= whitespace-separated list contains
    Includes,
    /// |= equals or starts with value-
    Dash,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttrSelector {
    pub name: String,
    pub op: AttrOp,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pseudo {
    Root,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    Empty,
    /// :nth-child(an+b); `last` counts from the end
    Nth { a: i32, b: i32, last: bool, of_type: bool },
    /// :not(list)
    Not(Vec<Compound>),
    /// :is / :where / :matches (list); `zero` = :where (no specificity)
    Is(Vec<Compound>, bool),
    /// :link / :any-link: an element with href
    Link,
    Disabled,
    Enabled,
    Checked,
    /// interaction states and pseudo-elements: a static page never matches them
    Never,
}

/// One compound selector: `div.card#main[href]:first-child`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSelector>,
    pub pseudos: Vec<Pseudo>,
    /// `::before` (1) or `::after` (2): the rule styles that generated box
    pub pseudo_element: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Compounds outermost first; `links[i]` joins `chain[i]` to `chain[i + 1]`.
    pub chain: Vec<Compound>,
    pub links: Vec<Combinator>,
}

impl Compound {
    fn specificity(&self) -> (u32, u32, u32) {
        let mut s = (self.id.is_some() as u32, (self.classes.len() + self.attrs.len()) as u32, self.tag.is_some() as u32);
        for pseudo in &self.pseudos {
            match pseudo {
                Pseudo::Not(list) | Pseudo::Is(list, false) => {
                    let best = list.iter().map(|c| c.specificity()).max().unwrap_or((0, 0, 0));
                    s = (s.0 + best.0, s.1 + best.1, s.2 + best.2);
                }
                Pseudo::Is(_, true) => {}
                _ => s.1 += 1,
            }
        }
        s
    }
}

impl Selector {
    pub fn parse(text: &str) -> Option<Selector> {
        let mut chain = Vec::new();
        let mut links = Vec::new();
        let mut pending: Option<Combinator> = None;
        for token in split_selector(text.trim())? {
            match token.as_str() {
                ">" => pending = Some(Combinator::Child),
                "+" => pending = Some(Combinator::Adjacent),
                "~" => pending = Some(Combinator::Sibling),
                compound => {
                    if !chain.is_empty() {
                        links.push(pending.take().unwrap_or(Combinator::Descendant));
                    } else if pending.is_some() {
                        return None; // leading combinator
                    }
                    chain.push(parse_compound(compound)?);
                }
            }
        }
        if chain.is_empty() || pending.is_some() {
            return None;
        }
        Some(Selector { chain, links })
    }

    /// ids × 100 + classes/attributes/pseudo-classes × 10 + types.
    pub fn specificity(&self) -> u32 {
        let (a, b, c) = self.chain.iter().fold((0, 0, 0), |acc, part| {
            let s = part.specificity();
            (acc.0 + s.0, acc.1 + s.1, acc.2 + s.2)
        });
        a * 100 + b * 10 + c
    }
}

/// Split a complex selector into compounds and combinators, keeping
/// brackets and parentheses whole.
fn split_selector(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let flush = |current: &mut String, out: &mut Vec<String>| {
        if !current.is_empty() {
            out.push(std::mem::take(current));
        }
    };
    for ch in text.chars() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' | '[' => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' => {
                depth -= 1;
                current.push(ch);
            }
            '>' | '+' | '~' if depth == 0 => {
                flush(&mut current, &mut out);
                out.push(ch.to_string());
            }
            c if c.is_whitespace() && depth == 0 => flush(&mut current, &mut out),
            c => current.push(c),
        }
    }
    flush(&mut current, &mut out);
    (depth == 0).then_some(out)
}

pub fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for ch in text.chars() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                current.push(ch);
            }
            '(' | '[' | '{' => {
                depth += 1;
                current.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(ch);
            }
            c if c == sep && depth == 0 => out.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    out.push(current);
    out
}

fn ident_end(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '-' || chars[i] == '_' || chars[i] == '\\' || !chars[i].is_ascii()) {
        if chars[i] == '\\' {
            i += 1;
        }
        i += 1;
    }
    i.min(chars.len())
}

fn unescape(s: &str) -> String {
    s.replace('\\', "")
}

fn parse_compound(text: &str) -> Option<Compound> {
    if text.trim().is_empty() { return None; }
    let chars: Vec<char> = text.chars().collect();
    let mut compound = Compound::default();
    let mut i = 0;
    if i < chars.len() && chars[i] == '*' {
        i += 1;
    } else if i < chars.len() && (chars[i].is_alphabetic() || chars[i] == '_') {
        let end = ident_end(&chars, i);
        compound.tag = Some(chars[i..end].iter().collect::<String>().to_ascii_lowercase());
        i = end;
    }
    while i < chars.len() {
        match chars[i] {
            '.' => {
                let end = ident_end(&chars, i + 1);
                if end == i + 1 {
                    return None;
                }
                compound.classes.push(unescape(&chars[i + 1..end].iter().collect::<String>()));
                i = end;
            }
            '#' => {
                let end = ident_end(&chars, i + 1);
                if end == i + 1 {
                    return None;
                }
                compound.id = Some(unescape(&chars[i + 1..end].iter().collect::<String>()));
                i = end;
            }
            '[' => {
                let close = chars[i..].iter().position(|c| *c == ']')? + i;
                let inner: String = chars[i + 1..close].iter().collect();
                compound.attrs.push(parse_attr(&inner)?);
                i = close + 1;
            }
            ':' => {
                let element = i + 1 < chars.len() && chars[i + 1] == ':';
                let start = if element { i + 2 } else { i + 1 };
                let end = ident_end(&chars, start);
                let name: String = chars[start..end].iter().collect::<String>().to_ascii_lowercase();
                let mut arg = None;
                i = end;
                if i < chars.len() && chars[i] == '(' {
                    let mut depth = 0;
                    let mut j = i;
                    while j < chars.len() {
                        match chars[j] {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    arg = Some(chars[i + 1..j.min(chars.len())].iter().collect::<String>());
                    i = j + 1;
                }
                // ::before / ::after: generated boxes (see Stylesheet::matching_pseudo)
                if name == "before" || name == "after" {
                    compound.pseudo_element = if name == "before" { 1 } else { 2 };
                    continue;
                }
                // ::placeholder, ::first-line…: not drawn
                if element || matches!(name.as_str(), "first-line" | "first-letter") {
                    compound.pseudos.push(Pseudo::Never);
                    continue;
                }
                compound.pseudos.push(parse_pseudo(&name, arg.as_deref())?);
            }
            _ => return None,
        }
    }
    Some(compound)
}

fn parse_attr(inner: &str) -> Option<AttrSelector> {
    let inner = inner.trim();
    let ops = [("~=", AttrOp::Includes), ("|=", AttrOp::Dash), ("^=", AttrOp::Prefix), ("$=", AttrOp::Suffix), ("*=", AttrOp::Substring), ("=", AttrOp::Equals)];
    for (token, op) in ops {
        if let Some((name, value)) = inner.split_once(token) {
            let value = value.trim().trim_end_matches(" i").trim_end_matches(" s").trim().trim_matches(|c| c == '"' || c == '\'');
            return Some(AttrSelector { name: name.trim().to_ascii_lowercase(), op, value: value.to_string() });
        }
    }
    Some(AttrSelector { name: inner.to_ascii_lowercase(), op: AttrOp::Exists, value: String::new() })
}

fn parse_nth(arg: &str) -> Option<(i32, i32)> {
    let s: String = arg.split(" of ").next()?.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some((a, b)) = s.split_once('n') {
        let a = match a {
            "" | "+" => 1,
            "-" => -1,
            v => v.parse().ok()?,
        };
        let b = if b.is_empty() { 0 } else { b.parse().ok()? };
        Some((a, b))
    } else {
        Some((0, s.parse().ok()?))
    }
}

fn parse_pseudo(name: &str, arg: Option<&str>) -> Option<Pseudo> {
    let list = |arg: Option<&str>| -> Option<Vec<Compound>> {
        // selector lists of compounds; complex selectors inside are not supported
        split_top_level(arg?, ',').iter().map(|s| parse_compound(s.trim())).collect()
    };
    Some(match name {
        "root" => Pseudo::Root,
        "first-child" => Pseudo::FirstChild,
        "last-child" => Pseudo::LastChild,
        "only-child" => Pseudo::OnlyChild,
        "first-of-type" => Pseudo::FirstOfType,
        "last-of-type" => Pseudo::LastOfType,
        "empty" => Pseudo::Empty,
        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
            let (a, b) = parse_nth(arg?)?;
            Pseudo::Nth { a, b, last: name.contains("last"), of_type: name.ends_with("of-type") }
        }
        "not" => Pseudo::Not(list(arg)?),
        "is" | "matches" | "any" | "-webkit-any" => Pseudo::Is(list(arg)?, false),
        "where" => Pseudo::Is(list(arg)?, true),
        "link" | "any-link" => Pseudo::Link,
        "disabled" => Pseudo::Disabled,
        "enabled" => Pseudo::Enabled,
        "checked" => Pseudo::Checked,
        // states a page without scripting or a pointer over it never shows
        "hover" | "focus" | "focus-visible" | "focus-within" | "active" | "visited" | "target" | "placeholder-shown"
        | "autofill" | "invalid" | "valid" | "required" | "optional" | "read-only" | "read-write" | "indeterminate"
        | "has" | "lang" | "dir" | "defined" | "fullscreen" | "modal" | "popover-open" | "open" | "closed" | "scope" => Pseudo::Never,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Stylesheets
// ---------------------------------------------------------------------------

/// A media condition, evaluated when matching (the viewport is known then).
#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    All,
    Never,
    /// (min-width) / (max-width) pairs, all must hold
    Width { min: Option<f32>, max: Option<f32> },
    Height { min: Option<f32>, max: Option<f32> },
    Not(Box<Media>),
    /// a comma list: any may hold
    Any(Vec<Media>),
    Both(Box<Media>, Box<Media>),
}

/// Whether the system is in dark mode (as browsers follow it):
/// SEPIA_COLOR_SCHEME=light|dark decides, else the macOS appearance.
pub fn system_dark() -> bool {
    static DARK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DARK.get_or_init(|| match std::env::var("SEPIA_COLOR_SCHEME").ok().as_deref() {
        Some("dark") => true,
        Some(_) => false,
        None if cfg!(target_os = "macos") => std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().eq_ignore_ascii_case("dark"))
            .unwrap_or(false),
        None => false,
    })
}

impl Media {
    pub fn parse(text: &str) -> Media {
        // minifiers drop the spaces: `(min-width:601px)and (max-width:1279px)`
        let text = text.trim().to_ascii_lowercase().replace(")and", ") and").replace(" and(", " and (").replace(")or", ") or").replace("not(", "not (");
        if text.is_empty() {
            return Media::All;
        }
        let alternatives: Vec<Media> = split_top_level(&text, ',').iter().map(|q| Media::parse_one(q.trim())).collect();
        if alternatives.len() == 1 {
            alternatives.into_iter().next().unwrap_or(Media::All)
        } else {
            Media::Any(alternatives)
        }
    }

    fn parse_one(query: &str) -> Media {
        let (negate, query) = match query.strip_prefix("not ") {
            Some(rest) => (true, rest),
            None => (false, query.strip_prefix("only ").unwrap_or(query)),
        };
        let mut result = Media::All;
        let mut min = None;
        let mut max = None;
        let mut min_height = None;
        let mut max_height = None;
        for part in query.split(" and ") {
            let part = part.trim();
            let feature = part.strip_prefix('(').and_then(|f| f.strip_suffix(')')).unwrap_or(part).trim();
            if part == "screen" || part == "all" {
                continue;
            }
            if part == "print" || part == "speech" {
                result = Media::Never;
                continue;
            }
            let (name, value) = match feature.split_once(':') {
                Some((n, v)) => (n.trim(), v.trim()),
                None => {
                    // range syntax: (width <= 600px), (width >= 600px)
                    if let Some((_, v)) = feature.split_once("<=").or_else(|| feature.split_once('<')) {
                        if feature.trim_start().starts_with("width") {
                            max = px_value(v.trim());
                            continue;
                        }
                    }
                    if let Some((_, v)) = feature.split_once(">=").or_else(|| feature.split_once('>')) {
                        if feature.trim_start().starts_with("width") {
                            min = px_value(v.trim());
                            continue;
                        }
                    }
                    if feature == "color" || feature == "pointer" || feature == "any-pointer" {
                        continue;
                    }
                    result = Media::Never;
                    continue;
                }
            };
            match name {
                // a length we cannot read makes the query false, as in browsers
                "min-width" | "max-width" | "min-height" | "max-height" => match px_value(value) {
                    Some(v) => match name {
                        "min-width" => min = Some(v),
                        "max-width" => max = Some(v),
                        "min-height" => min_height = Some(v),
                        _ => max_height = Some(v),
                    },
                    None => result = Media::Never,
                },
                // the scheme the user picked
                "prefers-color-scheme" if (value == "dark") != system_dark() => result = Media::Never,
                "prefers-color-scheme" => {}
                // a desktop browser tab as Chrome reports it: no accessibility
                // preferences, a landscape window, a mouse, 1x pixels; each
                // feature holds only for that value, so `prefers-contrast: more`
                // and `orientation: portrait` stay off
                _ => {
                    let holds = match name {
                        "prefers-reduced-motion" | "prefers-contrast" | "prefers-reduced-transparency" | "prefers-reduced-data" => value == "no-preference",
                        "orientation" => value == "landscape",
                        "pointer" | "any-pointer" => value == "fine",
                        "hover" | "any-hover" => value != "hover",
                        "display-mode" => value == "browser",
                        "forced-colors" | "inverted-colors" => value == "none",
                        "scripting" => value == "none",
                        "min-resolution" | "-webkit-min-device-pixel-ratio" | "min--moz-device-pixel-ratio" => {
                            let v = value.trim_end_matches("dppx").trim_end_matches('x');
                            match (v.parse::<f32>(), value.strip_suffix("dpi").and_then(|d| d.trim().parse::<f32>().ok())) {
                                (_, Some(dpi)) => dpi <= 96.0,
                                (Ok(r), _) => r <= 1.0,
                                _ => true,
                            }
                        }
                        _ => true,
                    };
                    if !holds {
                        result = Media::Never;
                    }
                }
            }
        }
        let dimensions = Media::Both(
            Box::new(Media::Width { min, max }),
            Box::new(Media::Height { min: min_height, max: max_height }),
        );
        let combined = if matches!(result, Media::Never) { Media::Never } else { dimensions };
        if negate { Media::Not(Box::new(combined)) } else { combined }
    }

    /// Compatibility entry point for callers that only supply a width.
    #[allow(dead_code)]
    pub fn matches(&self, width: f32) -> bool {
        self.matches_viewport(width, 800.0)
    }

    pub fn matches_viewport(&self, width: f32, height: f32) -> bool {
        match self {
            Media::All => true,
            Media::Never => false,
            Media::Width { min, max } => min.map_or(true, |m| width >= m) && max.map_or(true, |m| width <= m),
            Media::Height { min, max } => min.map_or(true, |m| height >= m) && max.map_or(true, |m| height <= m),
            Media::Not(query) => !query.matches_viewport(width, height),
            Media::Any(list) => list.iter().any(|m| m.matches_viewport(width, height)),
            Media::Both(a, b) => a.matches_viewport(width, height) && b.matches_viewport(width, height),
        }
    }
}

fn px_value(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(v) = value.strip_suffix("px") {
        v.trim().parse().ok()
    } else if let Some(v) = value.strip_suffix("rem").or_else(|| value.strip_suffix("em")) {
        v.trim().parse::<f32>().ok().map(|v| v * 16.0)
    } else if value.contains('(') {
        // calc(), min(), max(), clamp() over lengths, as in (max-width: calc(1120px - 1px))
        crate::style::parse_length(value, 16.0, 0.0)
    } else {
        value.parse().ok()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selector: Selector,
    pub declarations: Vec<Declaration>,
    pub media: Media,
    /// position in the cascade (later wins between equal specificity)
    pub order: usize,
}

/// An `@font-face`: a family name, its files (best first) and which face it is.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFace {
    /// lower case, unquoted
    pub family: String,
    /// `url()` sources in order, relative to the stylesheet until resolved
    pub sources: Vec<String>,
    pub bold: bool,
    pub italic: bool,
    /// worth loading first: no unicode-range, or one that covers basic Latin or Cyrillic
    pub common: bool,
    /// the `unicode-range` (empty: every character)
    pub ranges: Vec<(u32, u32)>,
}

impl FontFace {
    /// Whether the face is for any of these characters (as a browser only
    /// downloads the subsets a page's text needs).
    pub fn wanted(&self, chars: &std::collections::HashSet<char>) -> bool {
        self.ranges.is_empty() || chars.iter().any(|c| self.ranges.iter().any(|(a, b)| (*a..=*b).contains(&(*c as u32))))
    }
}

impl FontFace {
    fn parse(body: &str) -> Option<FontFace> {
        let mut face = FontFace { family: String::new(), sources: Vec::new(), bold: false, italic: false, common: true, ranges: Vec::new() };
        for d in parse_declarations(body) {
            match d.name.as_str() {
                "unicode-range" => {
                    let covers = |cp: u32| {
                        d.value.split(',').any(|r| {
                            let r = r.trim().trim_start_matches(['U', 'u']).trim_start_matches('+');
                            let (a, b) = match r.split_once('-') {
                                Some((a, b)) => (a.to_string(), b.to_string()),
                                None if r.contains('?') => (r.replace('?', "0"), r.replace('?', "F")),
                                None => (r.to_string(), r.to_string()),
                            };
                            match (u32::from_str_radix(&a, 16), u32::from_str_radix(&b, 16)) {
                                (Ok(a), Ok(b)) => (a..=b).contains(&cp),
                                _ => false,
                            }
                        })
                    };
                    face.common = covers(0x61) || covers(0x430);
                    for r in d.value.split(',') {
                        let r = r.trim().trim_start_matches(['U', 'u']).trim_start_matches('+');
                        let (a, b) = match r.split_once('-') {
                            Some((a, b)) => (a.to_string(), b.to_string()),
                            None if r.contains('?') => (r.replace('?', "0"), r.replace('?', "F")),
                            None => (r.to_string(), r.to_string()),
                        };
                        if let (Ok(a), Ok(b)) = (u32::from_str_radix(&a, 16), u32::from_str_radix(&b, 16)) {
                            face.ranges.push((a, b));
                        }
                    }
                }
                "font-family" => face.family = d.value.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase(),
                "src" => {
                    for source in split_top_level(&d.value, ',') {
                        let source = source.trim();
                        let lower = source.to_ascii_lowercase();
                        if !lower.starts_with("url(") {
                            continue;
                        }
                        let inner = &source[4..];
                        let url = inner.split(')').next().unwrap_or("").trim().trim_matches(|c| c == '"' || c == '\'');
                        if !url.is_empty() && !url.starts_with("data:") {
                            face.sources.push(url.to_string());
                        }
                    }
                }
                "font-weight" => {
                    let first = d.value.split_whitespace().next().unwrap_or("400").to_ascii_lowercase();
                    face.bold = first == "bold" || first == "bolder" || first.parse::<u32>().is_ok_and(|w| w >= 600);
                }
                "font-style" => face.italic = d.value.contains("italic") || d.value.contains("oblique"),
                _ => {}
            }
        }
        (!face.family.is_empty() && !face.sources.is_empty()).then_some(face)
    }
}

thread_local! {
    /// the root selector of the `@scope` being parsed
    static SCOPE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// `(root) to (limit)` → the root as one compound selector (`:is(...)` for a list).
fn scope_root(prelude: &str) -> Option<String> {
    let p = prelude.trim().strip_prefix('(')?;
    let mut depth = 1;
    let end = p.char_indices().find(|&(_, c)| { if c == '(' { depth += 1 } else if c == ')' { depth -= 1 } depth == 0 }).map(|(i, _)| i)?;
    let root = p[..end].trim();
    if root.is_empty() { return None; }
    Some(if root.contains(',') || root.contains(' ') || root.contains('>') { format!(":is({root})") } else { root.to_string() })
}

#[derive(Debug, Clone)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// `@font-face` rules, for the loader to fetch
    pub font_faces: Vec<FontFace>,
    /// `@import`s in order, for the loader to fetch: URL and the condition
    /// (media list) its rules apply under
    pub imports: Vec<(String, Media)>,
    /// `@keyframes` by name: the declarations of the last frame (`to` /
    /// `100%`), what an animation that fills forwards leaves on its element
    pub keyframes: std::collections::HashMap<String, Vec<Declaration>>,
    /// width the `@media` queries are evaluated against
    pub viewport_width: f32,
    /// Height used by viewport media conditions.
    pub viewport_height: f32,
    candidates: std::sync::OnceLock<RuleIndex>,
    pseudo_rules: std::sync::OnceLock<bool>,
    /// the ::before / ::after rules only, indexed like `candidates`
    pseudo_index: std::sync::OnceLock<RuleIndex>,
    /// those of them that set `content` (only they make a box exist)
    content_index: std::sync::OnceLock<RuleIndex>,
}

#[derive(Debug, Clone, Default)]
struct RuleIndex {
    len: usize,
    keys: std::collections::HashMap<(u8, String), Vec<usize>>,
    universal: Vec<usize>,
}

impl PartialEq for Stylesheet {
    fn eq(&self, other: &Self) -> bool {
        self.rules == other.rules && self.font_faces == other.font_faces
            && self.imports == other.imports && self.viewport_width == other.viewport_width
            && self.viewport_height == other.viewport_height
    }
}

impl Default for Stylesheet {
    fn default() -> Stylesheet {
        Stylesheet { rules: Vec::new(), font_faces: Vec::new(), imports: Vec::new(), keyframes: Default::default(), viewport_width: 1024.0, viewport_height: 800.0, candidates: Default::default(), pseudo_rules: Default::default(), pseudo_index: Default::default(), content_index: Default::default() }
    }
}

impl Stylesheet {
    pub fn parse(css: &str) -> Stylesheet {
        let mut sheet = Stylesheet::default();
        let css = strip_comments(css);
        sheet.parse_block(&css, &Media::All);
        sheet
    }

    /// Append another sheet after this one (later in the cascade).
    /// `url()`s in backgrounds are relative to the sheet they are written
    /// in: make them absolute so the page can fetch and key them as written.
    pub fn absolutize_urls(&mut self, base: &crate::url::Url) {
        for rule in &mut self.rules {
            for d in &mut rule.declarations {
                if d.name.starts_with("background") || d.name.starts_with("mask") || d.name.starts_with("-webkit-mask") {
                    if let Some(found) = css_url(&d.value) {
                        if !found.starts_with("data:") {
                            if let Some(abs) = base.resolve(&found) {
                                d.value = d.value.replacen(&found, &abs.to_string(), 1);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Every background image the page's elements use (each url once):
    /// from rules whose selector matches an element now, and inline styles.
    pub fn background_urls(&self, document: &crate::dom::Node) -> Vec<String> {
        let rules: Vec<(&Rule, String)> = self
            .rules
            .iter()
            .filter(|r| r.media.matches_viewport(self.viewport_width, self.viewport_height))
            .flat_map(|r| r.declarations.iter().filter(|d| is_image_property(&d.name)).filter_map(|d| css_url(&d.value)).map(move |u| (r, u)))
            .collect();
        let mut out: Vec<String> = Vec::new();
        fn walk<'n>(node: &'n crate::dom::Node, ancestors: &mut Vec<&'n crate::dom::Node>, rules: &[(&Rule, String)], out: &mut Vec<String>) {
            if matches!(node.tag.as_str(), "template" | "noscript") { return; }
            if node.text.is_none() && !matches!(node.tag.as_str(), "#document" | "head" | "script" | "style") {
                if let Some(inline) = node.attr("style").filter(|s| s.contains("url(")) {
                    for d in parse_declarations(inline) {
                        if is_image_property(&d.name) {
                            if let Some(u) = css_url(&d.value) {
                                if !out.contains(&u) {
                                    out.push(u);
                                }
                            }
                        }
                    }
                }
                for (rule, u) in rules {
                    if !out.contains(u) && matches_selector(&rule.selector, node, ancestors) {
                        out.push(u.clone());
                    }
                }
            }
            ancestors.push(node);
            for child in &node.children {
                walk(child, ancestors, rules, out);
            }
            ancestors.pop();
        }
        walk(document, &mut Vec::new(), &rules, &mut out);
        out
    }

    pub fn append(&mut self, other: Stylesheet) {
        self.candidates.take();
        self.pseudo_rules.take();
        self.pseudo_index.take();
        self.content_index.take();
        let base = self.rules.len();
        for mut rule in other.rules {
            rule.order += base;
            self.rules.push(rule);
        }
        self.imports.extend(other.imports);
        self.font_faces.extend(other.font_faces);
        // a later @keyframes of the same name replaces the earlier one
        self.keyframes.extend(other.keyframes);
    }

    fn parse_block(&mut self, css: &str, media: &Media) {
        let chars: Vec<char> = css.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ';' || chars[i] == '}') {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            // prelude up to `{` or a statement `;`
            let start = i;
            let mut quote: Option<char> = None;
            let mut depth = 0;
            while i < chars.len() {
                let c = chars[i];
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                } else if depth == 0 && (c == '{' || (c == ';' && chars[start] == '@')) {
                    break;
                }
                i += 1;
            }
            let prelude: String = chars[start..i.min(chars.len())].iter().collect();
            let prelude = prelude.trim().to_string();
            if i >= chars.len() {
                break;
            }
            if chars[i] == ';' {
                // statement at-rule: @import, @charset, @namespace, @layer a, b;
                if let Some(rest) = prelude.strip_prefix("@import") {
                    if matches!(media, Media::All) {
                        if let Some(import) = import_rule(rest, media) {
                            self.imports.push(import);
                        }
                    }
                }
                i += 1;
                continue;
            }
            // block: find its matching `}`
            let body_start = i + 1;
            let mut depth = 1;
            let mut quote: Option<char> = None;
            i += 1;
            while i < chars.len() && depth > 0 {
                let c = chars[i];
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                }
                i += 1;
            }
            let body: String = chars[body_start..i.saturating_sub(1).max(body_start)].iter().collect();
            if let Some(at) = prelude.strip_prefix('@') {
                let (name, condition) = at.split_once(|c: char| c.is_whitespace() || c == '(').map(|(n, _)| (n, &at[n.len()..])).unwrap_or((at, ""));
                match name.to_ascii_lowercase().as_str() {
                    "media" => {
                        let inner = Media::parse(condition);
                        let combined = match media {
                            Media::All => inner,
                            outer => Media::Both(Box::new(outer.clone()), Box::new(inner)),
                        };
                        self.parse_block(&body, &combined);
                    }
                    // feature queries: we take the first branch, as most sites
                    // write fallbacks outside and enhancements inside
                    // `@scope (root) { … }`: each rule applies inside `root` only
                    // (also how a shadow tree's stylesheet stays in its tree)
                    "scope" => {
                        let root = scope_root(condition);
                        let outer = SCOPE.with(|s| s.borrow().clone());
                        let combined = match (&outer, root) {
                            (Some(o), Some(r)) => Some(format!("{o} {r}")),
                            (o, r) => r.or(o.clone()),
                        };
                        SCOPE.with(|s| *s.borrow_mut() = combined);
                        self.parse_block(&body, media);
                        SCOPE.with(|s| *s.borrow_mut() = outer);
                    }
                    "supports" | "layer" | "container" | "document" | "-moz-document" | "starting-style" => {
                        if !condition.contains("not ") {
                            self.parse_block(&body, media);
                        }
                    }
                    "font-face" => {
                        if let Some(face) = FontFace::parse(&body) {
                            self.font_faces.push(face);
                        }
                    }
                    "keyframes" | "-webkit-keyframes" => {
                        let name = condition.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                        if let Some(last) = last_keyframe(&body) {
                            self.keyframes.insert(name, last);
                        }
                    }
                    // @page, @property…
                    other => crate::report::miss("css-at", &format!("@{other}")),
                }
                continue;
            }
            let declarations = parse_declarations(&body);
            if declarations.is_empty() {
                continue;
            }
            let scope = SCOPE.with(|s| s.borrow().clone());
            for selector_text in split_top_level(&prelude, ',') {
                let selector_text = match &scope {
                    Some(root) if selector_text.contains(":scope") => selector_text.replace(":scope", root),
                    Some(root) => format!("{root} {selector_text}"),
                    None => selector_text,
                };
                if let Some(selector) = Selector::parse(&selector_text) {
                    let order = self.rules.len();
                    self.rules.push(Rule { selector, declarations: declarations.clone(), media: media.clone(), order });
                } else {
                    crate::report::css_selector(&selector_text);
                }
            }
        }
    }

    /// Declarations that apply to `node`, in cascade order: normal ones by
    /// (specificity, source order), then `!important` ones the same way.
    /// `ancestors` runs outermost first and ends with the parent.
    pub fn matching(&self, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> std::rc::Rc<Vec<Declaration>> {
        LAYOUT_MATCHES.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(cache) = slot.as_mut() else { return std::rc::Rc::new(self.matching_uncached(node, ancestors)) };
            let shape = {
                use std::hash::{Hash, Hasher};
                let mut h = Fx::default();
                node.tag.hash(&mut h);
                node.attrs.hash(&mut h);
                for a in ancestors {
                    (*a as *const crate::dom::Node as usize).hash(&mut h);
                }
                h.finish()
            };
            let key = (self as *const Self as usize, node as *const crate::dom::Node as usize, shape, self.viewport_width.to_bits(), self.viewport_height.to_bits());
            if let Some(hit) = cache.get(&key) {
                return hit.clone();
            }
            let declarations = std::rc::Rc::new(self.matching_uncached(node, ancestors));
            cache.insert(key, declarations.clone());
            declarations
        })
    }

    fn matching_uncached(&self, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> Vec<Declaration> {
        self.matching_kind(node, ancestors, 0)
    }

    /// Declarations for `node`'s `::before` (1) or `::after` (2) box.
    pub fn matching_pseudo(&self, node: &crate::dom::Node, ancestors: &[&crate::dom::Node], kind: u8) -> Vec<Declaration> {
        if !self.has_pseudo_rules() {
            return Vec::new();
        }
        self.matching_kind(node, ancestors, kind)
    }

    /// Whether any rule styles a ::before / ::after box.
    pub fn has_pseudo_rules(&self) -> bool {
        *self.pseudo_rules.get_or_init(|| self.rules.iter().any(|r| r.selector.chain.last().is_some_and(|c| c.pseudo_element != 0)))
    }

    /// The `content` of `node`'s ::before (1) / ::after (2) box, if a rule gives one.
    pub fn pseudo_content(&self, node: &crate::dom::Node, ancestors: &[&crate::dom::Node], kind: u8) -> Option<String> {
        if !self.has_pseudo_rules() {
            return None;
        }
        self.matching_in(&self.content_index, true, node, ancestors, kind)
            .into_iter().rev().find(|d| d.name == "content").map(|d| d.value)
    }

    fn matching_kind(&self, node: &crate::dom::Node, ancestors: &[&crate::dom::Node], kind: u8) -> Vec<Declaration> {
        let slot = if kind == 0 { &self.candidates } else { &self.pseudo_index };
        self.matching_in(slot, false, node, ancestors, kind)
    }

    fn matching_in(&self, slot: &std::sync::OnceLock<RuleIndex>, content_only: bool, node: &crate::dom::Node, ancestors: &[&crate::dom::Node], kind: u8) -> Vec<Declaration> {
        let index = slot.get_or_init(|| {
            let mut index = RuleIndex { len: self.rules.len(), ..RuleIndex::default() };
            for (i, rule) in self.rules.iter().enumerate() {
                let Some(last) = rule.selector.chain.last() else { continue };
                if last.pseudos.iter().any(|p| matches!(p, Pseudo::Never)) { continue; }
                // each index holds only its own kind of rule
                if (last.pseudo_element != 0) != (kind != 0) { continue; }
                if content_only && !rule.declarations.iter().any(|d| d.name == "content") { continue; }
                let key = last.id.as_ref().map(|id| (0, id.clone()))
                    .or_else(|| last.classes.first().map(|class| (1, class.clone())))
                    .or_else(|| last.tag.as_ref().map(|tag| (2, tag.clone())));
                if let Some(key) = key { index.keys.entry(key).or_default().push(i); }
                else { index.universal.push(i); }
            }
            index
        });
        let mut candidates = index.universal.clone();
        let mut add = |kind, value: &str| {
            if let Some(indices) = index.keys.get(&(kind, value.to_string())) { candidates.extend(indices); }
        };
        add(2, &node.tag);
        if let Some(id) = node.attr("id") { add(0, id); }
        if let Some(classes) = node.attr("class") {
            for class in classes.split_whitespace() { add(1, class); }
        }
        // Public rule vectors may have been extended by an older caller.
        // Fall back to a full scan rather than silently dropping new rules.
        if index.len != self.rules.len() { candidates = (0..self.rules.len()).collect(); }
        candidates.sort_unstable();
        candidates.dedup();
        let mut hits: Vec<(u32, usize, &Rule)> = candidates.into_iter()
            .filter_map(|i| self.rules.get(i))
            .filter(|rule| rule.selector.chain.last().map_or(0, |c| c.pseudo_element) == kind)
            .filter(|rule| rule.media.matches_viewport(self.viewport_width, self.viewport_height) && if kind == 0 { matches_selector(&rule.selector, node, ancestors) } else { matches_for_pseudo(&rule.selector, node, ancestors) })
            .map(|rule| (rule.selector.specificity(), rule.order, rule))
            .collect();
        hits.sort_by_key(|(spec, order, _)| (*spec, *order));
        let mut normal = Vec::new();
        let mut important = Vec::new();
        for (_, _, rule) in hits {
            for d in &rule.declarations {
                if d.important {
                    important.push(d.clone());
                } else {
                    normal.push(d.clone());
                }
            }
        }
        normal.extend(important);
        normal
    }
}

/// The first `url(...)` in a value, quotes taken off.
/// Properties whose url() is an image the page draws: backgrounds and masks.
fn is_image_property(name: &str) -> bool {
    matches!(name, "background" | "background-image" | "mask" | "mask-image" | "-webkit-mask" | "-webkit-mask-image")
}

pub fn css_url(value: &str) -> Option<String> {
    let at = value.to_ascii_lowercase().find("url(")?;
    let inner = &value[at + 4..];
    let end = if inner.trim_start().starts_with(['"', '\'']) {
        let q = inner.trim_start().chars().next()?;
        let start = inner.find(q)? + 1;
        start + inner[start..].find(q)? + 1
    } else {
        inner.find(')')?
    };
    let url = inner[..end].trim().trim_matches(|c| c == '"' || c == '\'').trim();
    (!url.is_empty()).then(|| url.to_string())
}

/// `@import url(x) [layer(...)] [supports(...)] [media list]`: the URL
/// and the condition its rules apply under (with the enclosing one).
fn import_rule(rest: &str, outer: &Media) -> Option<(String, Media)> {
    let rest = rest.trim();
    let (target, after) = match rest.strip_prefix("url(") {
        Some(r) => {
            let (inside, after) = r.split_once(')')?;
            (inside, after)
        }
        None => {
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let end = rest[1..].find(quote)? + 1;
            (&rest[..=end], &rest[end + 1..])
        }
    };
    let url = target.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    if url.is_empty() {
        return None;
    }
    // cascade layers and feature queries before the media list: a layer
    // only orders rules; supports() is taken as met, as @supports is here
    let mut condition = after.trim();
    for prefix in ["layer", "supports"] {
        if let Some(r) = condition.strip_prefix(prefix) {
            condition = match r.strip_prefix('(') {
                Some(inner) => {
                    let mut depth = 1;
                    let mut end = inner.len();
                    for (i, c) in inner.char_indices() {
                        match c {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = i + 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    inner[end..].trim()
                }
                None if r.is_empty() || r.starts_with(char::is_whitespace) => r.trim(),
                None => condition,
            };
        }
    }
    let condition = condition.trim_end_matches(';').trim();
    let own = if condition.is_empty() { Media::All } else { Media::parse(condition) };
    let media = match (outer, own) {
        (Media::All, own) => own,
        (outer, Media::All) => outer.clone(),
        (outer, own) => Media::Both(Box::new(outer.clone()), Box::new(own)),
    };
    Some((url, media))
}

/// An imported sheet's rules apply only under its @import's condition.
pub fn under_media(sheet: &mut Stylesheet, media: &Media) {
    if matches!(media, Media::All) {
        return;
    }
    for rule in &mut sheet.rules {
        rule.media = match std::mem::replace(&mut rule.media, Media::All) {
            Media::All => media.clone(),
            own => Media::Both(Box::new(media.clone()), Box::new(own)),
        };
    }
    for (_, m) in &mut sheet.imports {
        *m = match std::mem::replace(m, Media::All) {
            Media::All => media.clone(),
            own => Media::Both(Box::new(media.clone()), Box::new(own)),
        };
    }
}

/// The declarations of a `@keyframes` block's last frame: the frame
/// whose selector list holds `to` or `100%` (merged when several do).
fn last_keyframe(body: &str) -> Option<Vec<Declaration>> {
    let mut out: Vec<Declaration> = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find('{') {
        let selector = rest[..open].trim().to_ascii_lowercase();
        let Some(close) = rest[open..].find('}') else { break };
        let block = &rest[open + 1..open + close];
        if selector.split(',').any(|s| matches!(s.trim(), "to" | "100%")) {
            out.extend(parse_declarations(block));
        }
        rest = &rest[open + close + 1..];
    }
    (!out.is_empty()).then_some(out)
}

pub fn parse_declarations(body: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in split_top_level(body, ';') {
        if let Some((name, value)) = part.split_once(':') {
            let name = name.trim();
            if name.is_empty() || name.contains(char::is_whitespace) {
                continue;
            }
            let mut value = value.trim().to_string();
            let mut important = false;
            if let Some(pos) = value.to_ascii_lowercase().rfind("!important") {
                value.truncate(pos);
                value = value.trim().to_string();
                important = true;
            }
            // `--x: ;` is a valid custom property (the empty "space toggle")
            if value.is_empty() && !name.starts_with("--") {
                continue;
            }
            // custom properties keep their case; everything else is case-insensitive
            let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
            out.push(Declaration { name, value, important });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

pub fn matches_selector(selector: &Selector, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> bool {
    // `p::before` selects a generated box, never the element itself
    if selector.chain.last().is_some_and(|c| c.pseudo_element != 0) {
        return false;
    }
    let last = selector.chain.len() - 1;
    match_from(selector, last, node, ancestors)
}

/// Does `selector` (ending in ::before / ::after) select `node`'s generated box?
fn matches_for_pseudo(selector: &Selector, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> bool {
    match_from(selector, selector.chain.len() - 1, node, ancestors)
}

/// Does `selector.chain[..=index]` match with `chain[index]` on `node`?
fn match_from(selector: &Selector, index: usize, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> bool {
    if !compound_matches(&selector.chain[index], node, ancestors) {
        return false;
    }
    if index == 0 {
        return true;
    }
    match selector.links[index - 1] {
        Combinator::Descendant => (0..ancestors.len()).rev().any(|i| match_from(selector, index - 1, ancestors[i], &ancestors[..i])),
        Combinator::Child => match ancestors.split_last() {
            Some((parent, rest)) => match_from(selector, index - 1, parent, rest),
            None => false,
        },
        Combinator::Adjacent => previous_elements(node, ancestors).first().is_some_and(|prev| match_from(selector, index - 1, prev, ancestors)),
        Combinator::Sibling => previous_elements(node, ancestors).iter().any(|prev| match_from(selector, index - 1, prev, ancestors)),
    }
}

/// Element siblings before `node`, nearest first.
fn previous_elements<'n>(node: &crate::dom::Node, ancestors: &[&'n crate::dom::Node]) -> Vec<&'n crate::dom::Node> {
    let Some(parent) = ancestors.last() else { return Vec::new() };
    let mut out = Vec::new();
    for child in &parent.children {
        if std::ptr::eq(child, node) {
            out.reverse();
            return out;
        }
        if child.text.is_none() && !child.tag.starts_with('#') {
            out.push(child);
        }
    }
    Vec::new()
}

/// (position, count) of `node` among its parent's element children (1-based),
/// optionally counting only same-tag siblings.
fn position(node: &crate::dom::Node, ancestors: &[&crate::dom::Node], of_type: bool) -> Option<(usize, usize)> {
    let parent = ancestors.last()?;
    let mut index = None;
    let mut count = 0;
    for child in parent.children.iter().filter(|c| c.text.is_none() && !c.tag.starts_with('#') && (!of_type || c.tag == node.tag)) {
        count += 1;
        if std::ptr::eq(child, node) {
            index = Some(count);
        }
    }
    index.map(|i| (i, count))
}

fn compound_matches(compound: &Compound, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> bool {
    if node.text.is_some() || node.tag.starts_with('#') {
        return false;
    }
    if let Some(tag) = &compound.tag {
        if !node.tag.eq_ignore_ascii_case(tag) {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        if node.attr("id") != Some(id.as_str()) {
            return false;
        }
    }
    if !compound.classes.is_empty() {
        let classes = node.attr("class").unwrap_or("");
        if !compound.classes.iter().all(|c| classes.split_whitespace().any(|have| have == c)) {
            return false;
        }
    }
    for attr in &compound.attrs {
        let Some(value) = node.attr(&attr.name) else { return false };
        let want = attr.value.as_str();
        let ok = match attr.op {
            AttrOp::Exists => true,
            AttrOp::Equals => value == want,
            AttrOp::Includes => value.split_whitespace().any(|v| v == want),
            AttrOp::Dash => value == want || value.starts_with(&format!("{want}-")),
            AttrOp::Prefix => !want.is_empty() && value.starts_with(want),
            AttrOp::Suffix => !want.is_empty() && value.ends_with(want),
            AttrOp::Substring => !want.is_empty() && value.contains(want),
        };
        if !ok {
            return false;
        }
    }
    for pseudo in &compound.pseudos {
        let ok = match pseudo {
            Pseudo::Never => false,
            Pseudo::Root => node.tag == "html",
            Pseudo::FirstChild => position(node, ancestors, false).is_some_and(|(i, _)| i == 1),
            Pseudo::LastChild => position(node, ancestors, false).is_some_and(|(i, n)| i == n),
            Pseudo::OnlyChild => position(node, ancestors, false).is_some_and(|(_, n)| n == 1),
            Pseudo::FirstOfType => position(node, ancestors, true).is_some_and(|(i, _)| i == 1),
            Pseudo::LastOfType => position(node, ancestors, true).is_some_and(|(i, n)| i == n),
            Pseudo::Empty => node.children.iter().all(|c| c.tag == "#comment" || c.text.as_ref().is_some_and(|t| t.is_empty())),
            Pseudo::Nth { a, b, last, of_type } => position(node, ancestors, *of_type).is_some_and(|(i, n)| {
                let k = if *last { (n - i + 1) as i32 } else { i as i32 };
                if *a == 0 {
                    k == *b
                } else {
                    (k - b) % a == 0 && (k - b) / a >= 0
                }
            }),
            Pseudo::Not(list) => !list.iter().any(|c| compound_matches(c, node, ancestors)),
            Pseudo::Is(list, _) => list.iter().any(|c| compound_matches(c, node, ancestors)),
            Pseudo::Link => matches!(node.tag.as_str(), "a" | "area") && node.attr("href").is_some(),
            Pseudo::Disabled => node.attr("disabled").is_some(),
            Pseudo::Enabled => matches!(node.tag.as_str(), "button" | "input" | "select" | "textarea") && node.attr("disabled").is_none(),
            Pseudo::Checked => node.attr("checked").is_some() || node.attr("selected").is_some(),
        };
        if !ok {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Custom properties
// ---------------------------------------------------------------------------

/// Where `var(--name)` looks a name up: the name's value with its own
/// `var()`s already replaced (None if missing or cyclic).
pub trait VarLookup {
    fn resolved(&self, name: &str, active: &mut Vec<String>) -> Option<String>;
}

/// A flat list, last definition winning (tests, small sets).
impl VarLookup for [(String, String)] {
    fn resolved(&self, name: &str, active: &mut Vec<String>) -> Option<String> {
        let raw = self.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())?;
        if raw.trim().eq_ignore_ascii_case("initial") {
            return None;
        }
        if active.iter().any(|a| a == name) {
            return None;
        }
        active.push(name.to_string());
        let out = resolve_vars(raw, self, active, active.len());
        active.pop();
        out
    }
}

impl VarLookup for Vec<(String, String)> {
    fn resolved(&self, name: &str, active: &mut Vec<String>) -> Option<String> {
        self.as_slice().resolved(name, active)
    }
}

/// The custom properties in force at an element: its own (as written), then
/// its ancestors'. A chain, so an element that sets a few does not copy the
/// thousands a design system puts on `:root`; a value is resolved the first
/// time it is used, in the scope that defines it, and remembered there.
#[derive(Debug, Default)]
pub struct Vars {
    own: std::collections::HashMap<String, String, FastHash>,
    parent: Option<std::sync::Arc<Vars>>,
    memo: std::sync::Mutex<std::collections::HashMap<String, Option<String>, FastHash>>,
}

impl PartialEq for Vars {
    fn eq(&self, other: &Vars) -> bool {
        std::ptr::eq(self, other) || (self.own == other.own && self.parent == other.parent)
    }
}

impl Vars {
    /// An element's scope on top of its parent's.
    pub fn child(parent: std::sync::Arc<Vars>) -> Vars {
        Vars { own: Default::default(), parent: Some(parent), memo: Default::default() }
    }

    pub fn set(&mut self, name: String, value: String) {
        self.own.insert(name, value);
    }
}

impl VarLookup for Vars {
    fn resolved(&self, name: &str, active: &mut Vec<String>) -> Option<String> {
        // the scope that defines it
        let mut scope = self;
        let raw = loop {
            if let Some(v) = scope.own.get(name) {
                break v;
            }
            scope = scope.parent.as_deref()?;
        };
        // `initial` is the guaranteed-invalid value: var() takes its fallback
        if raw.trim().eq_ignore_ascii_case("initial") {
            return None;
        }
        if let Some(done) = scope.memo.lock().ok().and_then(|m| m.get(name).cloned()) {
            return done;
        }
        if active.iter().any(|a| a == name) {
            return None;
        }
        active.push(name.to_string());
        let out = resolve_vars(raw, scope, active, active.len());
        active.pop();
        // a cycle seen from inside is not this value's final answer
        if active.is_empty() || out.is_some() {
            if let Ok(mut m) = scope.memo.lock() {
                m.insert(name.to_string(), out.clone());
            }
        }
        out
    }
}

/// Does a value use `var(`? (a byte scan: cheaper than setting up a substring
/// search for every declaration of every element)
pub fn has_var(value: &str) -> bool {
    value.as_bytes().windows(4).any(|w| w == b"var(")
}

/// Replace every `var(--name, fallback)` in `value` from `vars` (last
/// definition wins). Unresolvable references with no fallback make the
/// whole value invalid (None), as in CSS.
pub fn substitute_vars<V: VarLookup + ?Sized>(value: &str, vars: &V) -> Option<String> {
    resolve_vars(value, vars, &mut Vec::new(), 0)
}

fn resolve_vars<V: VarLookup + ?Sized>(value: &str, vars: &V, active: &mut Vec<String>, depth: usize) -> Option<String> {
    if depth >= 64 {
        return None;
    }
    if !has_var(value) {
        return Some(value.to_string());
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    let mut guard = 0;
    while let Some(start) = rest.find("var(") {
        guard += 1;
        if guard > 64 {
            return None;
        }
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        // matching parenthesis
        let mut level = 1;
        let mut end = None;
        for (i, c) in after.char_indices() {
            match c {
                '(' => level += 1,
                ')' => {
                    level -= 1;
                    if level == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &after[..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let resolved = vars.resolved(name, active);
        let resolved = resolved.or_else(|| fallback.and_then(|v| resolve_vars(v, vars, active, depth + 1)))?;
        out.push_str(&resolved);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            out.push(ch);
            if ch == '\\' {
                if let Some(escaped) = chars.next() { out.push(escaped); }
            } else if ch == q {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            out.push(ch);
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_do_not_count_as_element_siblings_or_empty_content() {
        let doc = crate::dom::parse_document("<div><!--before--><span>One</span><!--between--><b>Two</b><i><!--empty--></i></div>");
        let div = doc.find_first("div").unwrap();
        let sheet = Stylesheet::parse("span:first-child { color:red } span + b { color:blue } i:empty { color:green }");
        assert_eq!(values(&sheet, div.find_first("span").unwrap(), &[&doc, div]), vec!["red"]);
        assert_eq!(values(&sheet, div.find_first("b").unwrap(), &[&doc, div]), vec!["blue"]);
        assert_eq!(values(&sheet, div.find_first("i").unwrap(), &[&doc, div]), vec!["green"]);
    }

    #[test]
    fn indexed_rules_preserve_cascade_ancestors_media_and_append() {
        let doc = crate::dom::parse_document("<main class='outer'><p id='target' class='a a b' data-kind='x'>Text</p></main>");
        let main = doc.find_first("main").unwrap();
        let p = main.find_first("p").unwrap();
        let mut sheet = Stylesheet::parse("* { color:black } p { color:gray } .outer .a { color:red } #target { color:blue } [data-kind=x] { color:green !important } @media(max-width:600px) { .b { color:orange !important } } :is(.a,.b) { color:purple } p::before { color:pink }");
        for width in [500.0, 1200.0] {
            sheet.viewport_width = width;
            let mut rules: Vec<_> = sheet.rules.iter().filter(|r| r.media.matches_viewport(width, 800.0) && matches_selector(&r.selector, p, &[&doc, main])).collect();
            rules.sort_by_key(|r| (r.selector.specificity(), r.order));
            let expected: Vec<_> = rules.iter().flat_map(|r| r.declarations.iter()).filter(|d| !d.important)
                .chain(rules.iter().flat_map(|r| r.declarations.iter()).filter(|d| d.important)).cloned().collect();
            assert_eq!(*sheet.matching(p, &[&doc, main]), expected);
        }
        sheet.append(Stylesheet::parse("#target { background:red }"));
        assert!(sheet.matching(p, &[&doc, main]).iter().any(|d| d.name == "background"));
    }

    #[test]
    fn empty_functional_selectors_never_become_universal_rules() {
        let doc = crate::dom::parse_document("<body><p>Visible content</p></body>");
        let sheet = Stylesheet::parse(":is() { position:absolute; top:50%; left:50% } :where() { display:none } :not() { color:red } p { color:blue }");
        assert!(sheet.matching(&doc, &[]).is_empty());
        let body = doc.find_first("body").unwrap();
        assert!(sheet.matching(body, &[&doc]).is_empty());
        let p = body.find_first("p").unwrap();
        assert_eq!(values(&sheet, p, &[&doc, body]), vec!["blue".to_string()]);
    }

    #[test]
    fn cyclic_variables_invalidate_without_recursing_forever() {
        let vars = vec![("--a".into(), "var(--b)".into()), ("--b".into(), "var(--a)".into())];
        assert_eq!(substitute_vars("var(--a)", &vars), None);
        assert_eq!(substitute_vars("var(--a, 12px)", &vars), Some("12px".into()));
        assert_eq!(substitute_vars("var(--missing, rgb(1, 2, 3))", &vars), Some("rgb(1, 2, 3)".into()));
    }

    #[test]
    fn comments_inside_strings_and_invalid_unicode_colors_are_safe() {
        assert_eq!(strip_comments("a { content: '/*literal*/'; /* remove */ color: red }"), "a { content: '/*literal*/';  color: red }");
        assert_eq!(parse_color("#éab"), None);
        assert_eq!(parse_color("#界"), None);
    }
    use crate::dom::parse_document;

    fn values(sheet: &Stylesheet, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> Vec<String> {
        sheet.matching(node, ancestors).iter().map(|d| d.value.clone()).collect()
    }

    #[test]
    fn parses_rules_and_declarations() {
        let sheet = Stylesheet::parse("body { color: red; margin: 0 } .note, #main { background-color: #eee }");
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(sheet.rules[0].declarations[0].name, "color");
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    #[test]
    fn media_queries_follow_the_viewport() {
        let mut sheet = Stylesheet::parse(
            "p { color: black } @media (max-width: 600px) { p { color: red } } @media print { p { color: green } } \
             @media screen and (min-width: 900px) { p { color: blue } }",
        );
        let doc = parse_document("<p>t</p>");
        let p = doc.find_first("p").unwrap();
        sheet.viewport_width = 500.0;
        assert_eq!(values(&sheet, p, &[&doc]).last().unwrap(), "red");
        sheet.viewport_width = 1000.0;
        assert_eq!(values(&sheet, p, &[&doc]).last().unwrap(), "blue");
        assert!(!values(&sheet, p, &[&doc]).contains(&"green".to_string()), "print never applies");
    }

    #[test]
    fn specificity_order_and_important() {
        let sheet = Stylesheet::parse("p { color: red } .note { color: blue } #x { color: green } p { color: pink !important }");
        let doc = parse_document(r#"<p id="x" class="note">t</p>"#);
        let node = doc.find_first("p").unwrap();
        assert_eq!(values(&sheet, node, &[&doc]).last().unwrap(), "pink");
        assert_eq!(sheet.rules[0].selector.specificity(), 1);
        assert_eq!(sheet.rules[2].selector.specificity(), 100);
        let later = Stylesheet::parse(".a { color: red } .b { color: blue }");
        let doc = parse_document(r#"<p class="b a">t</p>"#);
        assert_eq!(values(&later, doc.find_first("p").unwrap(), &[&doc]).last().unwrap(), "blue", "source order breaks ties");
    }

    #[test]
    fn combinators_attributes_and_pseudo_classes() {
        let sheet = Stylesheet::parse(
            "ul > li { color: red } li + li { color: blue } h1 ~ p { color: green } a[href^=\"https\"] { color: teal } \
             li:first-child { font-weight: bold } li:nth-child(2n) { color: navy } li:not(.skip) { margin: 1px } \
             [role=tablist] { display: flex } a:hover { color: pink } p::after { content: 'x' }",
        );
        let doc = parse_document(
            "<div><ul><li>a</li><li class='skip'>b</li><li>c</li></ul><h1>t</h1><p>p</p><a href='https://x'>l</a><nav role='tablist'></nav></div>",
        );
        let div = doc.find_first("div").unwrap();
        let ul = div.find_first("ul").unwrap();
        let lis: Vec<&crate::dom::Node> = ul.children.iter().filter(|c| c.tag == "li").collect();
        let anc = [&doc, div, ul];
        let first = values(&sheet, lis[0], &anc);
        assert!(first.contains(&"red".into()) && first.contains(&"bold".into()) && first.contains(&"1px".into()));
        assert!(!first.contains(&"blue".into()), "first li has no previous li");
        let second = values(&sheet, lis[1], &anc);
        assert!(second.contains(&"blue".into()) && second.contains(&"navy".into()));
        assert!(!second.contains(&"1px".into()), ":not(.skip)");
        let p = div.find_first("p").unwrap();
        assert!(values(&sheet, p, &[&doc, div]).contains(&"green".into()));
        let a = div.find_first("a").unwrap();
        let link = values(&sheet, a, &[&doc, div]);
        assert!(link.contains(&"teal".into()) && !link.contains(&"pink".into()), "no :hover on a static page");
        assert!(values(&sheet, div.find_first("nav").unwrap(), &[&doc, div]).contains(&"flex".into()));
        assert!(!values(&sheet, p, &[&doc, div]).contains(&"'x'".into()), "pseudo-elements never match");
    }

    #[test]
    fn scoped_rules_stay_inside_their_root() {
        let doc = parse_document("<div><x-card><a>in</a></x-card><a>out</a></div>");
        let sheet = Stylesheet::parse("@scope (x-card) { a { display: grid } :scope { color: red } }");
        let div = doc.find_first("div").unwrap();
        let card = div.find_first("x-card").unwrap();
        let inner = card.find_first("a").unwrap();
        let outer = div.children.iter().filter(|c| c.tag == "a").last().unwrap();
        assert!(values(&sheet, inner, &[&doc, div, card]).contains(&"grid".into()));
        assert!(!values(&sheet, outer, &[&doc, div]).contains(&"grid".into()), "a scoped rule leaked out");
        assert!(values(&sheet, card, &[&doc, div]).contains(&"red".into()), ":scope is the root");
    }

    #[test]
    fn custom_properties() {
        let vars = vec![("--ink".to_string(), "#202c39".to_string()), ("--pad".to_string(), "4px var(--unit, 8px)".to_string())];
        assert_eq!(substitute_vars("1px solid var(--ink)", &vars).unwrap(), "1px solid #202c39");
        assert_eq!(substitute_vars("var(--pad)", &vars).unwrap(), "4px 8px");
        assert_eq!(substitute_vars("var(--missing, red)", &vars).unwrap(), "red");
        assert!(substitute_vars("var(--missing)", &vars).is_none());
    }

    #[test]
    fn space_toggle_custom_properties() {
        let decls = parse_declarations("--on: ; --off: initial");
        assert_eq!(decls.len(), 2, "an empty custom property is kept");
        let vars = vec![("--on".to_string(), "".to_string()), ("--off".to_string(), "initial".to_string())];
        assert_eq!(substitute_vars("var(--on) blue", &vars).unwrap().trim(), "blue");
        assert_eq!(substitute_vars("var(--off, red)", &vars).unwrap(), "red");
    }

    #[test]
    fn media_queries_without_spaces_around_and() {
        let q = Media::parse("(min-width:601px)and (max-width:1279.98px)");
        assert!(q.matches_viewport(1200.0, 800.0) && !q.matches_viewport(1400.0, 800.0));
        assert!(Media::parse("screen and(max-width:700px)").matches_viewport(600.0, 800.0));
    }

    #[test]
    fn preference_media_features_hold_for_one_value() {
        let on = |q: &str| Media::parse(q).matches_viewport(1280.0, 800.0);
        assert!(on("(prefers-reduced-motion: no-preference)") && !on("(prefers-reduced-motion: reduce)"));
        assert!(!on("(prefers-contrast: more)") && on("(prefers-contrast: no-preference)"));
        assert!(on("(orientation: landscape)") && !on("(orientation: portrait)"));
        assert!(on("(pointer: fine)") && !on("(pointer: coarse)"));
        assert!(!on("(display-mode: standalone)") && !on("(min-resolution: 2dppx)") && on("(min-resolution: 1dppx)"));
        assert!(!on("(-webkit-min-device-pixel-ratio: 1.5)"));
    }

    #[test]
    fn media_queries_read_calc_lengths() {
        let q = Media::parse("screen and (max-width:calc(1120px - 1px))");
        assert!(q.matches_viewport(1000.0, 800.0) && !q.matches_viewport(1200.0, 800.0));
        assert!(!Media::parse("(min-width: nonsense)").matches_viewport(1200.0, 800.0));
    }

    #[test]
    fn at_rules_and_imports() {
        let sheet = Stylesheet::parse(
            "@import url(\"base.css\"); @import 'print.css' print; @charset \"utf-8\"; \
             @font-face { font-family: X; src: url(x.ttf) } @keyframes spin { from { color: red } } \
             @supports (display: grid) { .g { display: grid } } p { color: red }",
        );
        // every @import is kept with its condition; print never applies on screen
        let urls: Vec<&str> = sheet.imports.iter().map(|(u, _)| u.as_str()).collect();
        assert_eq!(urls, vec!["base.css", "print.css"]);
        assert!(sheet.imports[0].1.matches_viewport(1200.0, 800.0));
        assert!(!sheet.imports[1].1.matches_viewport(1200.0, 800.0));
        let themed = Stylesheet::parse("@import url(/dark.css) (prefers-color-scheme: dark) and (not (prefers-contrast: more)); @import url(x.css) layer(base) supports(display: grid) screen;");
        assert_eq!(themed.imports.len(), 2);
        assert!(themed.imports[1].1.matches_viewport(1200.0, 800.0), "layer() and supports() before the media list");
        let selectors: Vec<String> = sheet.rules.iter().map(|r| format!("{:?}", r.selector.chain[0].tag.as_deref().or(r.selector.chain[0].classes.first().map(|s| s.as_str())))).collect();
        assert_eq!(sheet.rules.len(), 2, "{selectors:?}");
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(parse_color("#1a2b3c"), Some(Color::rgb(26, 43, 60)));
        assert_eq!(parse_color("#263b521f"), Some(Color::rgba(0x26, 0x3b, 0x52, 0x1f)));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(Color::rgb(1, 2, 3)));
        assert_eq!(parse_color("rgb(1 2 3 / 50%)"), Some(Color::rgba(1, 2, 3, 128)));
        assert_eq!(parse_color("rgba(0,0,0,.5)"), Some(Color::rgba(0, 0, 0, 128)));
        assert_eq!(parse_color("hsl(0, 100%, 50%)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("rebeccapurple"), Some(Color::rgb(0x66, 0x33, 0x99)));
        assert_eq!(parse_color("nope"), None);
    }
}

#[cfg(test)]
mod viewport_media_regressions {
    use super::*;

    #[test]
    fn height_conditions_use_actual_viewport_and_compose_with_width() {
        let query = Media::parse("screen and (min-width: 400px) and (max-height: 500px)");
        assert!(query.matches_viewport(640.0, 480.0));
        assert!(!query.matches_viewport(640.0, 900.0));
        assert!(!query.matches_viewport(320.0, 480.0));
        assert!(Media::parse("(min-height: 700px)").matches_viewport(640.0, 700.0));
        assert!(!Media::parse("(min-height: 700px)").matches_viewport(640.0, 699.0));
        assert!(Media::parse("not screen and (max-height: 500px)").matches_viewport(640.0, 900.0));
        assert!(Media::parse("(max-width: 300px), (max-height: 500px)").matches_viewport(640.0, 480.0));
    }
}
