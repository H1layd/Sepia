//! Computed style: user-agent defaults, inheritance, the cascade, shorthand boxes.

use std::sync::Arc;

use crate::css::{parse_color, parse_declarations, Color, Declaration, Stylesheet};
use crate::dom::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    ListItem,
    None,
    Table,
    /// thead / tbody / tfoot
    RowGroup,
    Row,
    Cell,
    Flex,
    Grid,
    /// inline-level box with a block inside (inline-block, inline-flex, inline-grid, form controls)
    InlineBlock,
}

/// How an inline-block lays out its own children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inner {
    Flow,
    Flex,
    Grid,
}

/// background-size
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    /// width, height: px, or a fraction of the box when negative (-0.5 = 50%); NaN = auto
    Sized(f32, f32),
}

/// One axis of background-position: a fraction of the free space (0 = start,
/// 1 = end) or px from the start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BgPos {
    Frac(f32),
    Px(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    /// sticky: stays in flow (we do not scroll-pin it) but contains absolutes
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Upper,
    Lower,
    Capitalize,
}

/// Where a colour stop sits on the gradient line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopAt {
    Auto,
    Px(f32),
    Fraction(f32),
}

/// `linear-gradient()`: angle in degrees (180 = to bottom) and colour stops.
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub angle: f32,
    pub stops: Vec<(Color, StopAt)>,
}

/// An outer `box-shadow`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

/// Grid placement of an item: `grid-column: span n` or `1 / -1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridSpan {
    Auto,
    Span(usize),
    /// spans every column of its row
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignItems {
    Stretch,
    Start,
    End,
    Center,
}

/// `line-height`: inherited as specified (a factor stays a factor, so a
/// child with a bigger font gets proportionally taller lines).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,
    Factor(f32),
    Px(f32),
}

/// The flexbox subset: container and item properties together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flex {
    pub direction: FlexDirection,
    pub wrap: bool,
    pub justify: Justify,
    pub align: AlignItems,
    pub row_gap: f32,
    pub column_gap: f32,
    pub grow: f32,
    pub shrink: f32,
    pub basis: Option<f32>,
    /// align-self on an item (None: the container's align-items)
    pub align_self: Option<AlignItems>,
}

impl Default for Flex {
    fn default() -> Flex {
        Flex {
            direction: FlexDirection::Row,
            wrap: false,
            justify: Justify::Start,
            align: AlignItems::Stretch,
            row_gap: 0.0,
            column_gap: 0.0,
            grow: 0.0,
            shrink: 1.0,
            basis: None,
            align_self: None,
        }
    }
}

/// `vertical-align` / `valign`, as far as table cells use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn all(value: f32) -> Edges {
        Edges { top: value, right: value, bottom: value, left: value }
    }
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub display: Display,
    pub color: Color,
    pub background: Option<Color>,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub align: Align,
    pub margin: Edges,
    pub padding: Edges,
    pub border_width: f32,
    pub border_color: Color,
    pub width: Option<f32>,
    /// the width depends on the container (a percentage): not a size of its own when measuring content
    pub width_relative: bool,
    /// CSS `fill` / `stroke` for SVG (inherited): None = not set,
    /// Some(None) = `none`, Some(Some(c)) = a colour (currentColor resolved)
    pub svg_fill: Option<Option<Color>>,
    pub svg_stroke: Option<Option<Color>>,
    /// display: table-caption (laid out as a block, at its table's width)
    pub caption: bool,
    /// display: contents — no box of its own; its children take its place
    pub contents: bool,
    pub height: Option<f32>,
    pub line_height: LineHeight,
    /// extra px after every character (inherited)
    pub letter_spacing: f32,
    /// first-line indent of a block's text, px (inherited)
    pub text_indent: f32,
    pub max_width: Option<f32>,
    /// `margin-left` / `margin-right: auto` (centring with a width or max-width)
    pub margin_auto: (bool, bool),
    pub pre: bool,
    pub marker: Option<&'static str>,
    /// list-style-type as inherited (set on a <ul>, it reaches its <li>s)
    pub list_marker: Option<&'static str>,
    /// None = not specified (a cell then takes its row's, else middle).
    pub valign: Option<VAlign>,
    pub border_collapse: bool,
    pub flex: Flex,
    /// inner layout of an InlineBlock
    pub inner: Inner,
    /// per-side border widths and colours (top, right, bottom, left)
    pub border: Edges,
    pub border_colors: [Color; 4],
    pub radius: f32,
    pub gradient: Option<Gradient>,
    pub shadow: Option<Shadow>,
    pub min_height: Option<f32>,
    pub min_width: Option<f32>,
    /// box-sizing: border-box (width includes padding and border)
    pub border_box: bool,
    pub text_transform: TextTransform,
    /// white-space: nowrap (inherited)
    pub nowrap: bool,
    /// visibility: hidden (inherited)
    pub hidden: bool,
    /// Local opacity, kept separate from inherited visibility.
    pub opacity: f32,
    /// This box or an ancestor has zero opacity (descendants cannot undo it).
    pub transparent: bool,
    pub position: Position,
    pub left: Option<f32>,
    pub top: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
    /// float: left / right (not inherited)
    pub float: Option<Align>,
    /// overflow hidden/clip on each axis: what spills out is not drawn
    pub clip_x: bool,
    pub clip_y: bool,
    /// clear: start below the floats before it
    pub clear: bool,
    /// mask-image: the background is only a colour for an icon shape we cannot cut out
    pub masked: bool,
    /// mask-image: the mask's URL, size and position (drawn as the
    /// background colour cut out by the image's alpha)
    pub mask_image: Option<Arc<str>>,
    pub mask_size: Option<BgSize>,
    pub mask_pos: Option<(BgPos, BgPos)>,
    /// the colour a mask is filled with (the box's background colour)
    pub mask_tint: Option<Color>,
    /// background-image url (absolute when it came from a stylesheet)
    pub bg_image: Option<Arc<str>>,
    pub bg_size: BgSize,
    pub bg_pos: (BgPos, BgPos),
    /// a transform that moves the box a whole box-length or more off its place
    pub moved_away: bool,
    /// grid-template-columns, resolved at layout (it needs the width)
    pub grid_columns: Option<String>,
    pub grid_span: GridSpan,
    pub grid_areas: Option<Vec<Vec<String>>>,
    pub grid_area: Option<String>,
    /// grid-column / grid-row as written (start, end): lines, names, spans
    pub grid_column_lines: Option<(String, String)>,
    /// aspect-ratio: width / height
    pub aspect_ratio: Option<f32>,
    /// `order` of a flex or grid item
    pub order: i32,
    pub grid_row_lines: Option<(String, String)>,
    /// custom properties in scope (inherited), last definition wins
    pub vars: Arc<crate::css::Vars>,
    /// font-family asks for a monospace face (inherited)
    pub monospace: bool,
    /// the used color scheme is dark (color-scheme allows it and the system is dark)
    pub dark: bool,
    /// the font-family list, lower case (inherited); fonts resolve it
    pub family: Option<Arc<str>>,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            display: Display::Inline,
            color: Color::BLACK,
            background: None,
            font_size: 16.0,
            bold: false,
            italic: false,
            underline: false,
            align: Align::Left,
            margin: Edges::default(),
            padding: Edges::default(),
            border_width: 0.0,
            border_color: Color::BLACK,
            width: None,
            height: None,
            line_height: LineHeight::Normal,
            letter_spacing: 0.0,
            text_indent: 0.0,
            max_width: None,
            margin_auto: (false, false),
            pre: false,
            marker: None,
            list_marker: None,
            valign: None,
            border_collapse: false,
            flex: Flex::default(),
            inner: Inner::Flow,
            border: Edges::default(),
            border_colors: [Color::BLACK; 4],
            radius: 0.0,
            gradient: None,
            shadow: None,
            min_height: None,
            min_width: None,
            border_box: false,
            text_transform: TextTransform::None,
            nowrap: false,
            hidden: false,
            opacity: 1.0,
            transparent: false,
            position: Position::Static,
            left: None,
            top: None,
            right: None,
            bottom: None,
            float: None,
            clip_x: false,
            clip_y: false,
            clear: false,
            masked: false,
            mask_image: None,
            mask_size: None,
            mask_pos: None,
            mask_tint: None,
            bg_image: None,
            bg_size: BgSize::Auto,
            bg_pos: (BgPos::Frac(0.0), BgPos::Frac(0.0)),
            moved_away: false,
            grid_columns: None,
            grid_span: GridSpan::Auto,
            grid_areas: None,
            grid_area: None,
            grid_column_lines: None,
            aspect_ratio: None,
            order: 0,
            grid_row_lines: None,
            vars: Arc::new(crate::css::Vars::default()),
            monospace: false,
            width_relative: false,
            svg_fill: None,
            svg_stroke: None,
            caption: false,
            contents: false,
            dark: false,
            family: None,
        }
    }
}

/// The user-agent stylesheet: what a page gets for free.
pub fn default_style(tag: &str) -> Style {
    let mut style = Style::default();
    match tag {
        "body" => {
            style.display = Display::Block;
            style.margin = Edges::all(8.0);
        }
        "html" | "div" | "section" | "article" | "header" | "footer" | "main"
        | "nav" | "aside" | "figure" | "figcaption" | "form" | "fieldset" | "address"
        | "blockquote" | "dl" | "dt" | "dd" | "caption" | "#document" => {
            style.display = Display::Block;
        }
        "p" => {
            style.display = Display::Block;
            style.margin = Edges { top: 16.0, right: 0.0, bottom: 16.0, left: 0.0 };
        }
        "h1" => {
            style.display = Display::Block;
            style.font_size = 32.0;
            style.bold = true;
            style.margin = Edges { top: 21.0, right: 0.0, bottom: 21.0, left: 0.0 };
        }
        "h2" => {
            style.display = Display::Block;
            style.font_size = 24.0;
            style.bold = true;
            style.margin = Edges { top: 19.0, right: 0.0, bottom: 19.0, left: 0.0 };
        }
        "h3" => {
            style.display = Display::Block;
            style.font_size = 18.7;
            style.bold = true;
            style.margin = Edges { top: 18.0, right: 0.0, bottom: 18.0, left: 0.0 };
        }
        "h4" | "h5" | "h6" => {
            style.display = Display::Block;
            style.font_size = 16.0;
            style.bold = true;
            style.margin = Edges { top: 18.0, right: 0.0, bottom: 18.0, left: 0.0 };
        }
        "ul" | "ol" | "menu" => {
            style.display = Display::Block;
            style.margin = Edges { top: 16.0, right: 0.0, bottom: 16.0, left: 0.0 };
            style.padding = Edges { top: 0.0, right: 0.0, bottom: 0.0, left: 40.0 };
        }
        "center" => {
            style.display = Display::Block;
            style.align = Align::Center;
        }
        "table" => style.display = Display::Table,
        "thead" | "tbody" | "tfoot" => style.display = Display::RowGroup,
        "tr" => style.display = Display::Row,
        // cell padding comes from the table's cellpadding (layout)
        "td" => style.display = Display::Cell,
        "th" => {
            style.display = Display::Cell;
            style.bold = true;
            style.align = Align::Center;
        }
        "li" => {
            style.display = Display::ListItem;
            style.marker = Some("•");
        }
        "a" => {
            style.color = Color::BLUE;
            style.underline = true;
        }
        "b" | "strong" => style.bold = true,
        "i" | "em" | "cite" | "var" => style.italic = true,
        "u" | "ins" => style.underline = true,
        "s" | "strike" | "del" => style.underline = true,
        "code" | "kbd" | "samp" | "tt" => {
            style.font_size = 14.0;
            style.monospace = true;
            style.family = Some(Arc::from("monospace"));
        }
        "pre" => {
            style.display = Display::Block;
            style.pre = true;
            style.nowrap = true;
            style.monospace = true;
            style.family = Some(Arc::from("monospace"));
            style.font_size = 14.0;
            style.margin = Edges { top: 13.0, right: 0.0, bottom: 13.0, left: 0.0 };
            style.background = Some(Color::rgb(245, 245, 245));
            style.padding = Edges::all(8.0);
        }
        "small" => style.font_size = 13.0,
        "big" => style.font_size = 20.0,
        "hr" => {
            style.display = Display::Block;
            set_border(&mut style, 0, Some(1.0), Some(Color::rgb(200, 200, 200)));
            style.margin = Edges { top: 8.0, right: 0.0, bottom: 8.0, left: 0.0 };
        }
        "br" => style.display = Display::Inline,
        "input" | "button" | "select" | "textarea" => {
            // form controls are inline-blocks with a frame, like every browser draws them
            style.display = Display::InlineBlock;
            for side in 0..4 {
                set_border(&mut style, side, Some(1.0), Some(Color::rgb(118, 118, 118)));
            }
            style.radius = 3.0;
            style.font_size = 13.3;
            if tag == "button" {
                style.background = Some(Color::rgb(239, 239, 239));
                style.padding = Edges { top: 1.0, right: 6.0, bottom: 1.0, left: 6.0 };
                style.align = Align::Center;
            } else {
                style.background = Some(Color::WHITE);
                style.padding = Edges { top: 2.0, right: 3.0, bottom: 2.0, left: 3.0 };
            }
        }
        "img" => style.display = Display::Inline,
        "head" | "style" | "script" | "title" | "meta" | "link" | "base" | "noscript" | "#comment" => {
            style.display = Display::None;
        }
        _ => {}
    }
    style
}

// Viewport size in CSS px, for vw / vh (set by the layout).
thread_local! {
    // Page workers lay out different tabs concurrently. One tab's viewport
    // must never change vw/vh used by another worker halfway through layout.
    static VIEWPORT: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((1024.0, 800.0)) };
    /// font-size of <html>: what rem is measured in (set as <html> is styled)
    static ROOT_FONT: std::cell::Cell<f32> = const { std::cell::Cell::new(16.0) };
    /// what a percentage height is of: the parent's definite height (the
    /// viewport for the root); None makes `height: 50%` auto, as in CSS
    static PERCENT_HEIGHT_BASE: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// `light-dark(a, b)`: the first in a light scheme, the second in a dark one.
fn light_dark(value: &str, dark: bool) -> String {
    let mut out = value.to_string();
    while let Some(start) = out.find("light-dark(") {
        let open = start + "light-dark(".len();
        let mut depth = 1;
        let mut end = None;
        for (i, ch) in out[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let args = crate::css::split_top_level(&out[open..end], ',');
        let pick = args.get(if dark { 1 } else { 0 }).or(args.first()).map(|a| a.trim().to_string()).unwrap_or_default();
        out.replace_range(start..=end, &pick);
    }
    out
}

/// A height-like length: percentages need a definite base, else none.
fn height_length(value: &str, font_size: f32) -> Option<f32> {
    if value.contains('%') {
        let base = PERCENT_HEIGHT_BASE.with(|b| b.get())?;
        parse_length(value, font_size, base)
    } else {
        parse_length(value, font_size, 0.0)
    }
}

pub fn root_font_size() -> f32 {
    ROOT_FONT.with(|r| r.get())
}

pub fn set_viewport(width: f32, height: f32) {
    VIEWPORT.with(|v| v.set((width, height)));
}

fn parse_bg_size(value: &str, font_size: f32) -> BgSize {
    let v = value.trim().to_ascii_lowercase();
    let words: Vec<&str> = v.split_whitespace().filter(|w| !w.contains("repeat") && !matches!(*w, "fixed" | "scroll" | "local")).collect();
    match words.first().copied() {
        Some("cover") => BgSize::Cover,
        Some("contain") => BgSize::Contain,
        None | Some("auto") if words.len() <= 1 => BgSize::Auto,
        _ => {
            let one = |w: Option<&&str>| -> f32 {
                match w {
                    Some(w) if w.ends_with('%') => -(w.trim_end_matches('%').parse::<f32>().unwrap_or(100.0) / 100.0),
                    Some(w) if *w != "auto" => parse_length(w, font_size, 0.0).unwrap_or(f32::NAN),
                    _ => f32::NAN,
                }
            };
            BgSize::Sized(one(words.first()), one(words.get(1)))
        }
    }
}

fn parse_bg_pos(value: &str, font_size: f32) -> Option<(BgPos, BgPos)> {
    let v = value.trim().to_ascii_lowercase();
    let mut x: Option<BgPos> = None;
    let mut y: Option<BgPos> = None;
    let mut loose: Vec<BgPos> = Vec::new();
    for w in v.split_whitespace() {
        match w {
            "left" => x = Some(BgPos::Frac(0.0)),
            "right" => x = Some(BgPos::Frac(1.0)),
            "top" => y = Some(BgPos::Frac(0.0)),
            "bottom" => y = Some(BgPos::Frac(1.0)),
            "center" => loose.push(BgPos::Frac(0.5)),
            w if w.ends_with('%') => loose.push(BgPos::Frac(w.trim_end_matches('%').parse::<f32>().ok()? / 100.0)),
            w => {
                if let Some(px) = parse_length(w, font_size, 0.0) {
                    loose.push(BgPos::Px(px));
                }
            }
        }
    }
    let mut loose = loose.into_iter();
    let x = x.or_else(|| loose.next());
    let y = y.or_else(|| loose.next());
    if x.is_none() && y.is_none() {
        return None;
    }
    Some((x.unwrap_or(BgPos::Frac(0.5)), y.unwrap_or(BgPos::Frac(0.5))))
}

/// Height of the first screen (what `position: fixed; bottom` is measured from).
pub fn viewport_height() -> f32 {
    VIEWPORT.with(|v| v.get().1)
}

/// Lengths: px, em, rem, %, vw/vh/vmin/vmax, pt, ch/ex, bare numbers as px,
/// and calc() / min() / max() / clamp() over them.
pub fn parse_length(value: &str, font_size: f32, percent_base: f32) -> Option<f32> {
    let value = value.trim().to_lowercase();
    if value == "0" {
        return Some(0.0);
    }
    for (prefix, kind) in [("calc(", 0), ("min(", 1), ("max(", 2), ("clamp(", 3), ("-webkit-calc(", 0)] {
        if let Some(inner) = value.strip_prefix(prefix).and_then(|v| v.strip_suffix(')')) {
            if kind == 0 {
                return eval_calc(inner, font_size, percent_base);
            }
            // each argument is a calc sum (`clamp(3rem, 2rem + 3vw, 5rem)`)
            let args: Vec<f32> = split_args(inner).iter().map(|a| parse_length(a, font_size, percent_base).or_else(|| eval_calc(a, font_size, percent_base))).collect::<Option<Vec<f32>>>()?;
            return match kind {
                1 => args.into_iter().reduce(f32::min),
                2 => args.into_iter().reduce(f32::max),
                _ if args.len() == 3 => Some(args[1].clamp(args[0], args[2].max(args[0]))),
                _ => None,
            };
        }
    }
    let (vw, vh) = VIEWPORT.with(|v| v.get());
    let unit = |suffix: &str| value.strip_suffix(suffix).and_then(|n| n.trim().parse::<f32>().ok());
    if let Some(n) = unit("vw") {
        return Some(n * vw / 100.0);
    }
    if let Some(n) = unit("vh").or_else(|| unit("dvh")).or_else(|| unit("svh")).or_else(|| unit("lvh")) {
        return Some(n * vh / 100.0);
    }
    if let Some(n) = unit("vmin") {
        return Some(n * vw.min(vh) / 100.0);
    }
    if let Some(n) = unit("vmax") {
        return Some(n * vw.max(vh) / 100.0);
    }
    if let Some(n) = unit("pt") {
        return Some(n * 4.0 / 3.0);
    }
    if let Some(n) = unit("ch").or_else(|| unit("ex")) {
        return Some(n * font_size * 0.5);
    }
    if let Some(px) = value.strip_suffix("px") {
        return px.trim().parse().ok();
    }
    // rem before em: "1rem" also ends in "em"
    if let Some(rem) = value.strip_suffix("rem") {
        let factor: f32 = rem.trim().parse().ok()?;
        return Some(factor * root_font_size());
    }
    if let Some(em) = value.strip_suffix("em") {
        let factor: f32 = em.trim().parse().ok()?;
        return Some(factor * font_size);
    }
    if let Some(pct) = value.strip_suffix('%') {
        let factor: f32 = pct.trim().parse().ok()?;
        return Some(factor / 100.0 * percent_base);
    }
    value.parse().ok()
}

fn split_args(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in inner.chars() {
        match c {
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    out.push(current);
    out.into_iter().map(|a| a.trim().to_string()).collect()
}

/// calc(): + - * / and parentheses over lengths and plain numbers.
fn eval_calc(expr: &str, font_size: f32, percent_base: f32) -> Option<f32> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
        font: f32,
        base: f32,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Option<f32> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'+') => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    Some(b'-') => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => return Some(v),
                }
            }
        }
        fn term(&mut self) -> Option<f32> {
            let mut v = self.factor()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'*') => {
                        self.i += 1;
                        v *= self.factor()?;
                    }
                    Some(b'/') => {
                        self.i += 1;
                        let d = self.factor()?;
                        if d == 0.0 {
                            return None;
                        }
                        v /= d;
                    }
                    _ => return Some(v),
                }
            }
        }
        fn factor(&mut self) -> Option<f32> {
            self.ws();
            if self.s.get(self.i) == Some(&b'(') {
                self.i += 1;
                let v = self.expr()?;
                self.ws();
                if self.s.get(self.i) == Some(&b')') {
                    self.i += 1;
                }
                return Some(v);
            }
            // a nested function or a dimension token
            let start = self.i;
            let mut depth = 0;
            while self.i < self.s.len() {
                let c = self.s[self.i];
                if c == b'(' {
                    depth += 1;
                } else if c == b')' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if depth == 0 && (c.is_ascii_whitespace() || c == b'*' || c == b'/' || ((c == b'+' || c == b'-') && self.i > start)) {
                    break;
                }
                self.i += 1;
            }
            let token = std::str::from_utf8(&self.s[start..self.i]).ok()?;
            if let Ok(n) = token.parse::<f32>() {
                return Some(n);
            }
            parse_length(token, self.font, self.base)
        }
    }
    let mut p = P { s: expr.as_bytes(), i: 0, font: font_size, base: percent_base };
    p.expr()
}

fn parse_four(value: &str, font_size: f32, percent_base: f32) -> Option<Edges> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let values: Vec<f32> = parts
        .iter()
        .map(|p| parse_length(p, font_size, percent_base))
        .collect::<Option<Vec<f32>>>()?;
    match values.len() {
        1 => Some(Edges::all(values[0])),
        2 => Some(Edges { top: values[0], right: values[1], bottom: values[0], left: values[1] }),
        3 => Some(Edges {
            top: values[0],
            right: values[1],
            bottom: values[2],
            left: values[1],
        }),
        4 => Some(Edges {
            top: values[0],
            right: values[1],
            bottom: values[2],
            left: values[3],
        }),
        _ => None,
    }
}

/// The properties a child inherits from `parent`, everything else default.
pub fn inherited(parent: &Style) -> Style {
    let mut style = Style::default();
    style.color = parent.color;
    style.font_size = parent.font_size;
    style.bold = parent.bold;
    style.italic = parent.italic;
    style.underline = false;
    style.align = parent.align;
    style.line_height = parent.line_height;
    style.letter_spacing = parent.letter_spacing;
    style.text_indent = parent.text_indent;
    style.text_transform = parent.text_transform;
    style.nowrap = parent.nowrap;
    style.hidden = parent.hidden;
    style.transparent = parent.transparent;
    style.vars = parent.vars.clone();
    style.monospace = parent.monospace;
    style.dark = parent.dark;
    style.svg_fill = parent.svg_fill;
    style.svg_stroke = parent.svg_stroke;
    style.family = parent.family.clone();
    style.pre = parent.pre;
    style
}

pub fn compute(
    node: &Node,
    parent: Option<&Style>,
    sheet: &Stylesheet,
    ancestors: &[&Node],
    containing_width: f32,
) -> Style {
    let mut style = default_style(&node.tag);
    // UA: `[popover]:not(:popover-open)` and `dialog:not([open])` are hidden
    if (node.attr("popover").is_some() && node.attr("data-sepia-popover-open").is_none()) || (node.tag == "dialog" && node.attr("open").is_none()) {
        style.display = Display::None;
    }
    let parent_size = parent.map(|p| p.font_size).unwrap_or(16.0);
    if let Some(parent_style) = parent {
        let inherit = inherited(parent_style);
        // UA sizes are relative to the parent (h1 is 2em), not absolute
        style.font_size = match ua_font_ratio(&node.tag) {
            Some(ratio) => parent_size * ratio,
            None => inherit.font_size,
        };
        if !style.bold {
            style.bold = inherit.bold;
        }
        if !style.italic {
            style.italic = inherit.italic;
        }
        style.color = inherit.color;
        style.align = inherit.align;
        style.line_height = inherit.line_height;
        style.letter_spacing = inherit.letter_spacing;
        style.text_indent = inherit.text_indent;
        style.text_transform = inherit.text_transform;
        style.nowrap = inherit.nowrap;
        style.hidden = inherit.hidden;
        style.transparent = inherit.transparent;
        style.vars = inherit.vars.clone();
        style.monospace = style.monospace || inherit.monospace;
        if style.family.is_none() {
            style.family = inherit.family.clone();
        }
        style.pre = style.pre || inherit.pre;
        style.list_marker = parent_style.list_marker;
        if style.display == Display::ListItem {
            if let Some(m) = parent_style.list_marker {
                style.marker = Some(m);
            }
        }
    }

    presentational_hints(node, &mut style, containing_width);

    // the root's own `rem` is the initial size, not what it computed last time
    if node.tag == "html" {
        ROOT_FONT.with(|r| r.set(16.0));
    }
    let matched = sheet.matching(node, ancestors);
    let inline: Vec<Declaration> = node.attr("style").map(parse_declarations).unwrap_or_default();
    // (declaration, from a stylesheet: its text lives as long as the layout)
    let declarations: Vec<(&Declaration, bool)> = matched.iter().map(|d| (d, true)).chain(inline.iter().map(|d| (d, false))).collect();
    // SEPIA_WHY=substring: print what reaches an element whose markup starts with it
    if let Ok(why) = std::env::var("SEPIA_WHY") {
        let head = format!("{} {:?}", node.tag, node.attrs);
        if head.contains(&why) {
            for (d, _) in &declarations {
                eprintln!("why {head}: {}: {}", d.name, d.value);
            }
        }
    }
    // custom properties first (they inherit), then everything else with var() resolved
    let mut own_vars: Vec<(String, String)> = Vec::new();
    for (declaration, _) in declarations.iter().filter(|(d, _)| d.name.starts_with("--")) {
        own_vars.push((declaration.name.clone(), declaration.value.clone()));
    }
    if !own_vars.is_empty() {
        // kept as written: a value is resolved when first used (css::Vars)
        let mut scope = crate::css::Vars::child(style.vars.clone());
        for (name, value) in own_vars {
            scope.set(name, value);
        }
        style.vars = Arc::new(scope);
    }
    let mut explicit_color = false;
    let base = if node.tag == "html" { Some(viewport_height()) } else { parent.and_then(|p| p.height) };
    PERCENT_HEIGHT_BASE.with(|b| b.set(base));
    for (declaration, shared) in declarations.iter().filter(|(d, _)| !d.name.starts_with("--")) {
        if let Some(value) = crate::css::substitute_in(&declaration.value, &style.vars, *shared) {
            let keyword = value.trim().to_ascii_lowercase();
            if keyword == "inherit" {
                if let Some(p) = parent {
                    inherit_property(&mut style, p, &declaration.name);
                }
                continue;
            }
            if keyword == "initial" || keyword == "unset" || keyword == "revert" {
                continue;
            }
            if declaration.name == "font-size" {
                // em and % in font-size are relative to the parent's size
                if let Some(size) = font_size_value(&keyword, parent_size) {
                    style.font_size = size.max(4.0);
                }
                continue;
            }
            if declaration.name == "color-scheme" {
                let words: Vec<&str> = keyword.split_whitespace().collect();
                style.dark = words.contains(&"dark") && (crate::css::system_dark() || !words.contains(&"light"));
                continue;
            }
            if declaration.name == "color" {
                explicit_color = true;
            }
            let value: std::borrow::Cow<str> = if value.contains("light-dark(") { light_dark(&value, style.dark).into() } else { value };
            apply(&mut style, &declaration.name, &value, containing_width);
        }
    }
    // a scheme switched to dark here gives the dark default text colour
    if style.dark && !explicit_color && parent.map_or(true, |p| !p.dark) {
        style.color = crate::css::Color::rgb(255, 255, 255);
    }
    // links keep the scheme's link colour (LinkText) unless the page sets one
    if style.dark && !explicit_color && node.tag == "a" && node.attr("href").is_some() {
        style.color = crate::css::Color::rgb(158, 158, 255);
    }
    // a <table> made display:block still holds rows: CSS wraps them in an
    // anonymous table, so it lays out as one
    if node.tag == "table" && matches!(style.display, Display::Block | Display::ListItem) {
        style.display = Display::Table;
    }
    style.transparent |= style.opacity == 0.0;
    if node.tag == "html" {
        ROOT_FONT.with(|r| r.set(style.font_size.clamp(4.0, 200.0)));
    }
    if style.contents {
        // no box: nothing of its own to draw or to take room
        style.margin = Edges::default();
        style.padding = Edges::default();
        style.border = Edges::default();
        style.border_width = 0.0;
        style.background = None;
        style.gradient = None;
        style.bg_image = None;
        style.shadow = None;
        style.width = None;
        style.height = None;
        style.min_width = None;
        style.min_height = None;
        style.max_width = None;
    }
    if style.masked {
        // the mask image is drawn in the box's background colour; without a
        // usable image nothing is drawn (a solid square would read as a glitch)
        style.mask_tint = style.background.filter(|_| style.mask_image.is_some());
        if style.mask_tint.is_some() {
            style.bg_image = style.mask_image.clone();
            style.bg_size = style.mask_size.unwrap_or(BgSize::Auto);
            style.bg_pos = style.mask_pos.unwrap_or((BgPos::Frac(0.0), BgPos::Frac(0.0)));
        }
        style.background = None;
        style.gradient = None;
    }
    if style.position == Position::Absolute || style.position == Position::Fixed {
        // Out-of-flow boxes we cannot place: screen-reader-only text (1px,
        // clipped) and skip links parked off-screen stay invisible.
        let tiny = style.width.is_some_and(|w| w <= 1.0) || style.height.is_some_and(|h| h <= 1.0);
        let offscreen = style.left.is_some_and(|l| l < -100.0) || style.top.is_some_and(|t| t < -100.0) || style.moved_away;
        if tiny || offscreen {
            style.display = Display::None;
        }
    }
    style
}

/// Old HTML attributes that still style much of the web (HN is built on
/// them). They rank below any CSS, so they are applied first.
fn presentational_hints(node: &Node, style: &mut Style, containing_width: f32) {
    let tag = node.tag.as_str();
    if node.attr("hidden").is_some() {
        style.display = Display::None;
    }
    if tag == "th" {
        style.align = Align::Center; // not inherited from the row
    }
    if tag == "table" {
        // HTML's UA sheet (quirks): a table does not inherit text-align, so
        // <center><table> centres the table, not the text in its cells.
        style.align = Align::Left;
    }
    if let Some(color) = node.attr("bgcolor").and_then(parse_color) {
        style.background = Some(color);
    }
    if matches!(tag, "table" | "td" | "th" | "col" | "hr") {
        if let Some(width) = node.attr("width").and_then(|v| parse_length(v.trim(), style.font_size, containing_width)) {
            style.width = Some(width);
        }
    }
    if matches!(tag, "table" | "td" | "th" | "tr") {
        if let Some(height) = node.attr("height").and_then(|v| parse_length(v.trim(), style.font_size, containing_width)) {
            style.height = Some(height);
        }
    }
    // `align` on a table positions the table itself (layout reads it there).
    if matches!(tag, "td" | "th" | "tr" | "tbody" | "thead" | "tfoot" | "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        match node.attr("align").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("center") | Some("middle") => style.align = Align::Center,
            Some("right") => style.align = Align::Right,
            Some("left") => style.align = Align::Left,
            _ => {}
        }
    }
    if let Some(valign) = node.attr("valign").and_then(|v| parse_valign(&v.trim().to_ascii_lowercase())) {
        style.valign = Some(valign);
    }
}

/// The user-agent font size of `tag` as a fraction of its parent's.
fn ua_font_ratio(tag: &str) -> Option<f32> {
    Some(match tag {
        "h1" => 2.0,
        "h2" => 1.5,
        "h3" => 1.17,
        "h5" => 0.83,
        "h6" => 0.67,
        "small" => 0.83,
        "big" => 1.2,
        "code" | "kbd" | "samp" | "tt" | "pre" => 0.875,
        "sub" | "sup" => 0.83,
        "input" | "button" | "select" | "textarea" => 0.833,
        _ => return None,
    })
}

/// A `font-size` value against the parent's size: lengths, %, em,
/// keywords (small, larger…), and math.
fn font_size_value(value: &str, parent: f32) -> Option<f32> {
    let keyword = match value {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "xxx-large" => Some(48.0),
        "smaller" => Some(parent / 1.2),
        "larger" => Some(parent * 1.2),
        _ => None,
    };
    keyword.or_else(|| parse_length(value, parent, parent))
}

/// `property: inherit` (form controls use `font: inherit` to drop their UA font).
fn inherit_property(style: &mut Style, parent: &Style, name: &str) {
    match name {
        "font" => {
            style.font_size = parent.font_size;
            style.bold = parent.bold;
            style.italic = parent.italic;
            style.line_height = parent.line_height;
            style.monospace = parent.monospace;
            style.family = parent.family.clone();
        }
        "font-size" => style.font_size = parent.font_size,
        "font-weight" => style.bold = parent.bold,
        "font-style" => style.italic = parent.italic,
        "font-family" => {
            style.monospace = parent.monospace;
            style.family = parent.family.clone();
        }
        "line-height" => style.line_height = parent.line_height,
        "color" => style.color = parent.color,
        "visibility" => style.hidden = parent.hidden,
        "opacity" => style.opacity = parent.opacity,
        "text-align" => style.align = parent.align,
        // `*, *::before { box-sizing: inherit }` under `html { border-box }`
        "box-sizing" => style.border_box = parent.border_box,
        "list-style" | "list-style-type" => {
            style.list_marker = parent.list_marker;
            style.marker = parent.list_marker;
        }
        "background" | "background-color" => {
            style.background = parent.background;
            style.gradient = parent.gradient.clone();
        }
        _ => {}
    }
}

/// Which sides a border property names: "border" -> all, "border-top" -> top…
fn sides(name: &str) -> Vec<usize> {
    match name {
        "border-top" => vec![0],
        "border-right" => vec![1],
        "border-bottom" => vec![2],
        "border-left" => vec![3],
        _ => vec![0, 1, 2, 3],
    }
}

fn set_border(style: &mut Style, side: usize, width: Option<f32>, color: Option<Color>) {
    if let Some(w) = width {
        match side {
            0 => style.border.top = w,
            1 => style.border.right = w,
            2 => style.border.bottom = w,
            _ => style.border.left = w,
        }
    }
    if let Some(c) = color {
        style.border_colors[side] = c;
    }
    // the uniform fields tables use: the widest side and the last colour
    style.border_width = style.border.top.max(style.border.right).max(style.border.bottom).max(style.border.left);
    if let Some(c) = color {
        style.border_color = c;
    }
}

/// Split on top-level whitespace, keeping function arguments together.
fn split_space(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in value.chars() {
        match c {
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth -= 1;
                current.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn is_monospace(family: &str) -> bool {
    let f = family.to_ascii_lowercase();
    ["monospace", "mono", "courier", "consolas", "menlo", "monaco", "terminal"].iter().any(|m| f.contains(m))
}

/// Colour stops of a gradient's argument list (after the opening parenthesis).
fn gradient_stops(args: &str) -> Vec<(Color, StopAt)> {
    let inner = match args.rfind(')') {
        Some(end) => &args[..end],
        None => args,
    };
    let mut stops = Vec::new();
    for arg in split_args(inner) {
        let tokens = split_space(&arg);
        let Some(color) = tokens.first().and_then(|t| parse_color(t)) else { continue };
        let mut positions: Vec<StopAt> = tokens[1..]
            .iter()
            .filter_map(|t| {
                if let Some(p) = t.strip_suffix('%') {
                    p.parse::<f32>().ok().map(|v| StopAt::Fraction(v / 100.0))
                } else {
                    parse_length(t, 16.0, 0.0).map(StopAt::Px)
                }
            })
            .collect();
        if positions.is_empty() {
            positions.push(StopAt::Auto);
        }
        for at in positions {
            stops.push((color, at));
        }
    }
    stops
}

/// `linear-gradient(...)` (also repeating-, drawn once) anywhere in a background value.
pub fn parse_gradient(value: &str) -> Option<Gradient> {
    let start = value.find("linear-gradient(")?;
    let args = &value[start + "linear-gradient(".len()..];
    // the gradient's own closing parenthesis
    let mut depth = 1;
    let mut end = args.len();
    for (i, c) in args.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &args[..end];
    let parts = split_args(inner);
    let first = parts.first()?.trim().to_ascii_lowercase();
    let (angle, skip) = if let Some(deg) = first.strip_suffix("deg") {
        (deg.trim().parse::<f32>().ok()?, 1)
    } else if let Some(turn) = first.strip_suffix("turn") {
        (turn.trim().parse::<f32>().ok()? * 360.0, 1)
    } else if let Some(side) = first.strip_prefix("to ") {
        let a = match side.trim() {
            "top" => 0.0,
            "right" => 90.0,
            "bottom" => 180.0,
            "left" => 270.0,
            "top right" | "right top" => 45.0,
            "bottom right" | "right bottom" => 135.0,
            "bottom left" | "left bottom" => 225.0,
            "top left" | "left top" => 315.0,
            _ => 180.0,
        };
        (a, 1)
    } else {
        (180.0, 0)
    };
    let stops = gradient_stops(&(parts[skip..].join(",") + ")"));
    (stops.len() >= 1).then_some(Gradient { angle, stops })
}

fn parse_shadow(value: &str, font_size: f32) -> Option<Shadow> {
    if value == "none" {
        return None;
    }
    for layer in split_args(value) {
        let tokens = split_space(&layer);
        if tokens.iter().any(|t| t == "inset") {
            continue;
        }
        let mut lengths = Vec::new();
        let mut color = Color::rgba(0, 0, 0, 64);
        for t in &tokens {
            if let Some(c) = parse_color(t) {
                color = c;
            } else if let Some(l) = parse_length(t, font_size, 0.0) {
                lengths.push(l);
            }
        }
        if lengths.len() >= 2 {
            return Some(Shadow {
                dx: lengths[0],
                dy: lengths[1],
                blur: lengths.get(2).copied().unwrap_or(0.0),
                spread: lengths.get(3).copied().unwrap_or(0.0),
                color,
            });
        }
    }
    None
}

fn parse_valign(value: &str) -> Option<VAlign> {
    match value {
        "top" | "text-top" | "baseline" => Some(VAlign::Top),
        "middle" | "center" => Some(VAlign::Middle),
        "bottom" | "text-bottom" => Some(VAlign::Bottom),
        _ => None,
    }
}

fn parse_grid_areas(value: &str) -> Option<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if !matches!(ch, '\'' | '"') { continue; }
        let mut row = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == ch { closed = true; break; }
            row.push(c);
        }
        if !closed { return None; }
        let row: Vec<String> = row.split_whitespace().map(|name| {
            if name.chars().all(|c| c == '.') { ".".to_string() } else { name.to_string() }
        }).collect();
        if row.is_empty() || row.len() > 64 || rows.len() >= 128 { return None; }
        if rows.first().is_some_and(|first: &Vec<String>| first.len() != row.len()) { return None; }
        rows.push(row);
    }
    if rows.is_empty() { return None; }
    let mut bounds = std::collections::HashMap::<&str, (usize, usize, usize, usize)>::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, name) in row.iter().enumerate().filter(|(_, name)| name.as_str() != ".") {
            if !name.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-')) { return None; }
            let area = bounds.entry(name).or_insert((r, c, r, c));
            area.0 = area.0.min(r); area.1 = area.1.min(c);
            area.2 = area.2.max(r); area.3 = area.3.max(c);
        }
    }
    for (name, (r0, c0, r1, c1)) in bounds {
        if rows[r0..=r1].iter().any(|row| row[c0..=c1].iter().any(|cell| cell != name)) { return None; }
    }
    Some(rows)
}

/// Logical properties (`margin-inline`, `padding-block`, `inline-size`,
/// `inset-inline-start`…) as the physical ones they mean in horizontal,
/// left-to-right writing. None when `name` is not one.
fn logical(name: &str, value: &str) -> Option<Vec<(String, String)>> {
    let pair = |a: &str, b: &str| -> Vec<(String, String)> {
        let parts: Vec<String> = split_space(value);
        let first = parts.first().cloned().unwrap_or_default();
        let second = parts.get(1).cloned().unwrap_or_else(|| first.clone());
        vec![(a.to_string(), first), (b.to_string(), second)]
    };
    let one = |n: &str| vec![(n.to_string(), value.to_string())];
    Some(match name {
        "inline-size" => one("width"),
        "block-size" => one("height"),
        "min-inline-size" => one("min-width"),
        "max-inline-size" => one("max-width"),
        "min-block-size" => one("min-height"),
        "max-block-size" => one("max-height"),
        "margin-inline" => pair("margin-left", "margin-right"),
        "margin-block" => pair("margin-top", "margin-bottom"),
        "margin-inline-start" => one("margin-left"),
        "margin-inline-end" => one("margin-right"),
        "margin-block-start" => one("margin-top"),
        "margin-block-end" => one("margin-bottom"),
        "padding-inline" => pair("padding-left", "padding-right"),
        "padding-block" => pair("padding-top", "padding-bottom"),
        "padding-inline-start" => one("padding-left"),
        "padding-inline-end" => one("padding-right"),
        "padding-block-start" => one("padding-top"),
        "padding-block-end" => one("padding-bottom"),
        "inset-inline" => pair("left", "right"),
        "inset-block" => pair("top", "bottom"),
        "inset-inline-start" => one("left"),
        "inset-inline-end" => one("right"),
        "inset-block-start" => one("top"),
        "inset-block-end" => one("bottom"),
        "border-inline" => vec![("border-left".into(), value.into()), ("border-right".into(), value.into())],
        "border-block" => vec![("border-top".into(), value.into()), ("border-bottom".into(), value.into())],
        "border-inline-start" => one("border-left"),
        "border-inline-end" => one("border-right"),
        "border-block-start" => one("border-top"),
        "border-block-end" => one("border-bottom"),
        "border-inline-width" => pair("border-left-width", "border-right-width"),
        "border-block-width" => pair("border-top-width", "border-bottom-width"),
        "border-inline-color" => pair("border-left-color", "border-right-color"),
        "border-block-color" => pair("border-top-color", "border-bottom-color"),
        "border-inline-start-width" => one("border-left-width"),
        "border-inline-end-width" => one("border-right-width"),
        "border-block-start-width" => one("border-top-width"),
        "border-block-end-width" => one("border-bottom-width"),
        "border-inline-start-color" => one("border-left-color"),
        "border-inline-end-color" => one("border-right-color"),
        "border-block-start-color" => one("border-top-color"),
        "border-block-end-color" => one("border-bottom-color"),
        "border-start-start-radius" => one("border-top-left-radius"),
        "border-start-end-radius" => one("border-top-right-radius"),
        "border-end-start-radius" => one("border-bottom-left-radius"),
        "border-end-end-radius" => one("border-bottom-right-radius"),
        "overflow-inline" => one("overflow-x"),
        "overflow-block" => one("overflow-y"),
        _ => return None,
    })
}

fn apply(style: &mut Style, name: &str, value: &str, containing_width: f32) {
    if let Some(physical) = logical(name, value) {
        for (n, v) in physical {
            apply(style, &n, &v, containing_width);
        }
        return;
    }
    let value = value.trim();
    let font_size = style.font_size;
    match name {
        "display" => {
            style.caption = value == "table-caption";
            style.contents = value == "contents";
            style.display = match value {
                "none" => Display::None,
                "flex" => Display::Flex,
                "grid" => Display::Grid,
                "inline-block" | "inline-flex" | "inline-grid" | "inline-flow-root" => {
                    style.inner = match value {
                        "inline-flex" => Inner::Flex,
                        "inline-grid" => Inner::Grid,
                        _ => Inner::Flow,
                    };
                    Display::InlineBlock
                }
                "block" | "flow-root" | "contents" => Display::Block,
                "table" | "inline-table" => Display::Table,
                "table-row-group" | "table-header-group" | "table-footer-group" => Display::RowGroup,
                "table-row" => Display::Row,
                "table-cell" => Display::Cell,
                "list-item" => Display::ListItem,
                _ => Display::Inline,
            }
        }
        "color" => {
            if let Some(color) = parse_color(value) {
                style.color = color;
            }
        }
        "background-color" => {
            if value.eq_ignore_ascii_case("currentcolor") {
                style.background = Some(style.color);
            } else if let Some(color) = parse_color(value) {
                style.background = Some(color);
            }
        }
        "background-size" => style.bg_size = parse_bg_size(value, font_size),
        "background-position" => {
            if let Some(pos) = parse_bg_pos(value, font_size) {
                style.bg_pos = pos;
            }
        }
        "background" | "background-image" => {
            // a gradient layer, an image and/or a colour
            if value == "none" || value == "transparent" {
                if name == "background" {
                    style.background = None;
                }
                style.gradient = None;
                style.bg_image = None;
                return;
            }
            style.bg_image = crate::css::css_url(value).map(|u| Arc::from(u.as_str()));
            if name == "background" && style.bg_image.is_some() {
                // `url() center / cover no-repeat`: position and size ride along
                let rest = match value.rfind(')') {
                    Some(at) => &value[at + 1..],
                    None => value,
                };
                let (pos, size) = match rest.split_once('/') {
                    Some((p, s)) => (p, Some(s)),
                    None => (rest, None),
                };
                style.bg_pos = parse_bg_pos(pos, font_size).unwrap_or((BgPos::Frac(0.0), BgPos::Frac(0.0)));
                style.bg_size = size.map(|s| parse_bg_size(s, font_size)).unwrap_or(BgSize::Auto);
            }
            if let Some(gradient) = parse_gradient(value) {
                style.gradient = Some(gradient);
            } else if name == "background-image" {
                style.gradient = None;
            }
            if name == "background" {
                // the colour is the last layer's token that parses as one
                let color = split_space(value).iter().rev().find_map(|t| parse_color(t));
                style.background = color;
                if let Some(radial) = value.find("radial-gradient(") {
                    // drawn as its first colour
                    if style.background.is_none() {
                        style.background = gradient_stops(&value[radial + 16..]).first().map(|(c, _)| *c);
                    }
                }
            }
        }
        "font-family" => {
            style.monospace = is_monospace(value);
            style.family = Some(Arc::from(value.to_ascii_lowercase().as_str()));
        }
        "font" => {
            // [style] [weight] size[/line-height] family
            if value == "inherit" || value.starts_with("var(") {
                return;
            }
            let tokens = split_space(value);
            for (i, token) in tokens.iter().enumerate() {
                let (size, line) = match token.split_once('/') {
                    Some((s, l)) => (s, Some(l)),
                    None => (token.as_str(), None),
                };
                let keyword_size = match size {
                    "small" => Some(13.0),
                    "medium" => Some(16.0),
                    "large" => Some(18.0),
                    "x-large" => Some(24.0),
                    _ => None,
                };
                // a bare number is a weight (`font: 300 16px …`), never a size
                let unitless = size.parse::<f32>().is_ok();
                if let Some(px) = keyword_size.or_else(|| if !unitless && size.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') { parse_length(size, font_size, font_size) } else { None }) {
                    style.font_size = px.max(4.0);
                    if let Some(line) = line {
                        apply(style, "line-height", line, containing_width);
                    }
                    let family = tokens[i + 1..].join(" ");
                    style.monospace = is_monospace(&family);
                    if !family.trim().is_empty() {
                        style.family = Some(Arc::from(family.to_ascii_lowercase().as_str()));
                    }
                    break;
                }
                match token.as_str() {
                    "italic" | "oblique" => style.italic = true,
                    "bold" | "bolder" => style.bold = true,
                    "normal" => {}
                    w if w.parse::<u32>().is_ok() => style.bold = w.parse::<u32>().unwrap_or(400) >= 600,
                    _ => {}
                }
            }
        }
        "border-radius" => {
            let first = split_space(value).into_iter().next().unwrap_or_default();
            let first = first.split('/').next().unwrap_or("").trim().to_string();
            style.radius = if first.ends_with('%') {
                // 50% of an unknown box: as round as the box allows
                if first.trim_end_matches('%').parse::<f32>().unwrap_or(0.0) >= 50.0 { 9999.0 } else { 6.0 }
            } else {
                parse_length(&first, font_size, containing_width).unwrap_or(0.0)
            };
        }
        "box-shadow" => style.shadow = parse_shadow(value, font_size),
        "min-height" => style.min_height = height_length(value, font_size),
        "min-width" => style.min_width = parse_length(value, font_size, containing_width),
        "box-sizing" => style.border_box = value == "border-box",
        "text-transform" => {
            style.text_transform = match value {
                "uppercase" => TextTransform::Upper,
                "lowercase" => TextTransform::Lower,
                "capitalize" => TextTransform::Capitalize,
                _ => TextTransform::None,
            }
        }
        "visibility" => style.hidden = value == "hidden" || value == "collapse",
        // fully transparent things (checkbox hacks, fade-in content before JS) are not drawn
        "opacity" => {
            if let Ok(opacity) = value.trim_end_matches('%').parse::<f32>() {
                if opacity.is_finite() {
                    let scale = if value.ends_with('%') { 100.0 } else { 1.0 };
                    style.opacity = (opacity / scale).clamp(0.0, 1.0);
                }
            }
        }
        "position" => {
            style.position = match value {
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "relative" => Position::Relative,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => Position::Static,
            }
        }
        "left" => style.left = parse_length(value, font_size, containing_width),
        "right" => style.right = parse_length(value, font_size, containing_width),
        "overflow" => {
            let mut parts = value.split_whitespace();
            let x = parts.next().unwrap_or("");
            let y = parts.next().unwrap_or(x);
            style.clip_x = matches!(x, "hidden" | "clip");
            style.clip_y = matches!(y, "hidden" | "clip");
        }
        "mask-image" | "-webkit-mask-image" | "mask" | "-webkit-mask" => {
            style.masked = value.contains("url(");
            style.mask_image = crate::css::css_url(value).map(|u| Arc::from(u.as_str()));
            if name.ends_with("mask") {
                // the shorthand: `url() center / contain no-repeat`
                if let Some((_, size)) = value.split_once('/') {
                    style.mask_size = Some(parse_bg_size(size.split_whitespace().take(2).collect::<Vec<_>>().join(" ").as_str(), font_size));
                }
            }
        }
        "mask-size" | "-webkit-mask-size" => style.mask_size = Some(parse_bg_size(value, font_size)),
        "mask-position" | "-webkit-mask-position" => style.mask_pos = parse_bg_pos(value, font_size),
        "clear" => style.clear = matches!(value, "both" | "left" | "right" | "inline-start" | "inline-end"),
        "overflow-x" => style.clip_x = matches!(value, "hidden" | "clip"),
        "overflow-y" => style.clip_y = matches!(value, "hidden" | "clip"),
        "float" => {
            style.float = match value {
                "left" | "inline-start" => Some(Align::Left),
                "right" | "inline-end" => Some(Align::Right),
                _ => None,
            }
        }
        "bottom" => style.bottom = parse_length(value, font_size, containing_width),
        "inset" => {
            let parts: Vec<Option<f32>> = value.split_whitespace().map(|v| parse_length(v, font_size, containing_width)).collect();
            let at = |i: usize| parts.get(i).copied().flatten();
            let (t, r, b, l) = match parts.len() {
                1 => (at(0), at(0), at(0), at(0)),
                2 => (at(0), at(1), at(0), at(1)),
                3 => (at(0), at(1), at(2), at(1)),
                _ => (at(0), at(1), at(2), at(3)),
            };
            (style.top, style.right, style.bottom, style.left) = (t, r, b, l);
        }
        "transform" => {
            // translate(-100%…) and friends park skip links and drawers off-screen
            let v = value.replace(' ', "");
            style.moved_away = ["translatey(-", "translatex(-", "translate(-", "translate(0,-", "translate3d(0,-", "translate3d(-"]
                .iter()
                .any(|p| {
                    v.to_ascii_lowercase().find(p).is_some_and(|at| {
                        let rest = &v[at + p.len()..];
                        let number: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                        let n = number.parse::<f32>().unwrap_or(0.0);
                        let unit = &rest[number.len()..];
                        (unit.starts_with('%') && n >= 100.0) || (unit.starts_with("px") && n >= 500.0)
                    })
                });
        }
        "top" => style.top = parse_length(value, font_size, containing_width),
        "grid-template-columns" => style.grid_columns = (value != "none").then(|| value.to_string()),
        "grid-template-areas" => {
            if value == "none" { style.grid_areas = None; }
            else if let Some(areas) = parse_grid_areas(value) { style.grid_areas = Some(areas); }
        }
        "grid-template" | "grid" => {
            if value == "none" {
                style.grid_areas = None;
                style.grid_columns = None;
            } else if let Some(areas) = parse_grid_areas(value) {
                // A slash outside area strings separates rows from columns.
                let mut quote = None;
                let mut slash = None;
                for (i, ch) in value.char_indices() {
                    match (quote, ch) {
                        (Some(q), c) if c == q => quote = None,
                        (None, '\'' | '"') => quote = Some(ch),
                        (None, '/') => { slash = Some(i); break; }
                        _ => {}
                    }
                }
                style.grid_columns = slash.map(|i| value[i + 1..].trim().to_string());
                style.grid_areas = Some(areas);
            } else if let Some((_, columns)) = value.split_once('/') {
                // rows / columns, no areas: the columns are what we lay out by
                if !columns.contains("auto-flow") {
                    style.grid_columns = Some(columns.trim().to_string());
                }
            }
        }
        "grid-area" => {
            if value == "auto" { style.grid_area = None; }
            else if !value.is_empty() && value.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'))
                && !value.chars().next().unwrap().is_ascii_digit() {
                style.grid_area = Some(value.to_string());
            }
        }
        "order" => style.order = value.parse::<i32>().unwrap_or(0),
        "aspect-ratio" => {
            let v = value.replace("auto", "");
            let (a, b) = v.split_once('/').unwrap_or((v.as_str(), "1"));
            style.aspect_ratio = match (a.trim().parse::<f32>(), b.trim().parse::<f32>()) {
                (Ok(a), Ok(b)) if a > 0.0 && b > 0.0 => Some(a / b),
                _ => None,
            };
        }
        "grid-row" | "grid-row-start" | "grid-row-end" | "grid-column-start" | "grid-column-end" => {
            let slot = if name.starts_with("grid-row") { &mut style.grid_row_lines } else { &mut style.grid_column_lines };
            let pair = slot.get_or_insert_with(|| ("auto".to_string(), "auto".to_string()));
            if name.ends_with("-start") {
                pair.0 = value.trim().to_string();
            } else if name.ends_with("-end") {
                pair.1 = value.trim().to_string();
            } else {
                let (a, b) = value.split_once('/').unwrap_or((value, "auto"));
                *pair = (a.trim().to_string(), b.trim().to_string());
            }
        }
        "grid-column" => {
            let (a, b) = value.split_once('/').unwrap_or((value, "auto"));
            style.grid_column_lines = (value.trim() != "auto").then(|| (a.trim().to_string(), b.trim().to_string()));
            let v = value.replace(' ', "");
            style.grid_span = if v == "1/-1" || v.ends_with("/-1") && v.starts_with('1') {
                GridSpan::Full
            } else if let Some(n) = v.strip_prefix("span").and_then(|n| n.split('/').next().and_then(|n| n.parse::<usize>().ok())) {
                GridSpan::Span(n.max(1))
            } else {
                GridSpan::Auto
            };
        }
        "font-size" => {
            if let Some(size) = parse_length(value, font_size, font_size) {
                style.font_size = size.max(4.0);
            }
        }
        "font-weight" => {
            style.bold = match value {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                other => other.parse::<u32>().map(|w| w >= 600).unwrap_or(style.bold),
            }
        }
        "font-style" => style.italic = value == "italic" || value == "oblique",
        "text-decoration" | "text-decoration-line" => {
            style.underline = value.contains("underline")
        }
        "flex-direction" => {
            style.flex.direction = match value {
                "row-reverse" => FlexDirection::RowReverse,
                "column" => FlexDirection::Column,
                "column-reverse" => FlexDirection::ColumnReverse,
                _ => FlexDirection::Row,
            }
        }
        "flex-wrap" => style.flex.wrap = value.starts_with("wrap"),
        "flex-flow" => {
            for part in value.split_whitespace() {
                apply(style, if part.contains("wrap") { "flex-wrap" } else { "flex-direction" }, part, containing_width);
            }
        }
        "justify-content" => {
            style.flex.justify = match value.trim_start_matches("safe ").trim_start_matches("unsafe ") {
                "flex-end" | "end" | "right" => Justify::End,
                "center" => Justify::Center,
                "space-between" => Justify::SpaceBetween,
                "space-around" => Justify::SpaceAround,
                "space-evenly" => Justify::SpaceEvenly,
                _ => Justify::Start,
            }
        }
        "align-self" => {
            style.flex.align_self = match value {
                "auto" | "normal" => None,
                "flex-start" | "start" | "self-start" | "baseline" => Some(AlignItems::Start),
                "flex-end" | "end" | "self-end" => Some(AlignItems::End),
                "center" => Some(AlignItems::Center),
                _ => Some(AlignItems::Stretch),
            }
        }
        "align-items" => {
            style.flex.align = match value {
                "flex-start" | "start" | "self-start" | "baseline" | "first baseline" => AlignItems::Start,
                "flex-end" | "end" | "self-end" => AlignItems::End,
                "center" => AlignItems::Center,
                _ => AlignItems::Stretch,
            }
        }
        "gap" | "grid-gap" => {
            let mut parts = value.split_whitespace().filter_map(|v| parse_length(v, font_size, containing_width));
            if let Some(row) = parts.next() {
                style.flex.row_gap = row;
                style.flex.column_gap = parts.next().unwrap_or(row);
            }
        }
        "row-gap" => style.flex.row_gap = parse_length(value, font_size, containing_width).unwrap_or(0.0),
        "column-gap" => style.flex.column_gap = parse_length(value, font_size, containing_width).unwrap_or(0.0),
        "flex-grow" => style.flex.grow = value.parse::<f32>().unwrap_or(0.0).max(0.0),
        "flex-shrink" => style.flex.shrink = value.parse::<f32>().unwrap_or(1.0).max(0.0),
        "flex-basis" => style.flex.basis = parse_length(value, font_size, containing_width),
        "flex" => {
            // flex: none | auto | <grow> [<shrink>] [<basis>]
            let (grow, shrink, basis) = match value {
                "none" => (0.0, 0.0, None),
                "auto" => (1.0, 1.0, None),
                "initial" => (0.0, 1.0, None),
                _ => {
                    let mut numbers = Vec::new();
                    let mut basis = None;
                    let mut basis_given = false;
                    for part in value.split_whitespace() {
                        match part.parse::<f32>() {
                            Ok(n) if numbers.len() < 2 => numbers.push(n),
                            _ => {
                                basis_given = true;
                                basis = if part == "auto" { None } else { parse_length(part, font_size, containing_width) };
                            }
                        }
                    }
                    let grow = numbers.first().copied().unwrap_or(1.0);
                    let shrink = numbers.get(1).copied().unwrap_or(1.0);
                    // `flex: 1` means a basis of 0; `flex: 1 0 auto` keeps auto
                    let basis = if basis_given { basis } else { Some(0.0) };
                    (grow, shrink, basis)
                }
            };
            style.flex.grow = grow.max(0.0);
            style.flex.shrink = shrink.max(0.0);
            style.flex.basis = basis;
        }
        "vertical-align" => {
            style.valign = parse_valign(value).or(style.valign);
        }
        "border-collapse" => style.border_collapse = value == "collapse",
        "text-align" => {
            style.align = match value {
                "center" => Align::Center,
                "right" => Align::Right,
                _ => Align::Left,
            }
        }
        "margin" => {
            // `auto` counts as 0 for the box and is remembered for centring
            let parts: Vec<&str> = value.split_whitespace().collect();
            let (right, left) = match parts.len() {
                1 => (parts[0], parts[0]),
                2 | 3 => (parts[1], parts[1]),
                4 => (parts[1], parts[3]),
                _ => ("", ""),
            };
            let zeroed: Vec<&str> = parts.iter().map(|p| if *p == "auto" { "0" } else { p }).collect();
            if let Some(edges) = parse_four(&zeroed.join(" "), font_size, containing_width) {
                style.margin = edges;
                style.margin_auto = (left == "auto", right == "auto");
            }
        }
        "margin-left" | "margin-right" if value == "auto" => {
            if name == "margin-left" {
                style.margin.left = 0.0;
                style.margin_auto.0 = true;
            } else {
                style.margin.right = 0.0;
                style.margin_auto.1 = true;
            }
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            if let Some(v) = parse_length(value, font_size, containing_width) {
                match name {
                    "margin-top" => style.margin.top = v,
                    "margin-right" => {
                        style.margin.right = v;
                        style.margin_auto.1 = false;
                    }
                    "margin-bottom" => style.margin.bottom = v,
                    _ => {
                        style.margin.left = v;
                        style.margin_auto.0 = false;
                    }
                }
            }
        }
        "padding" => {
            if let Some(edges) = parse_four(value, font_size, containing_width) {
                style.padding = edges;
            }
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => {
            if let Some(v) = parse_length(value, font_size, containing_width) {
                match name {
                    "padding-top" => style.padding.top = v,
                    "padding-right" => style.padding.right = v,
                    "padding-bottom" => style.padding.bottom = v,
                    _ => style.padding.left = v,
                }
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            // width style colour, any order; without a visible style there is no border
            let mut width = 3.0;
            let mut visible = false;
            let mut color = style.color;
            for part in split_space(value) {
                match part.as_str() {
                    "none" | "hidden" => visible = false,
                    "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" => visible = true,
                    "thin" => width = 1.0,
                    "medium" => width = 3.0,
                    "thick" => width = 5.0,
                    p => {
                        if let Some(c) = parse_color(p) {
                            color = c;
                        } else if p.eq_ignore_ascii_case("currentcolor") {
                            color = style.color;
                        } else if let Some(w) = parse_length(p, font_size, containing_width) {
                            width = w;
                            if w == 0.0 {
                                visible = false;
                            }
                        }
                    }
                }
            }
            if value == "0" {
                visible = false;
            }
            let w = if visible { width } else { 0.0 };
            for side in sides(name) {
                set_border(style, side, Some(w), Some(color));
            }
        }
        "border-width" | "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => {
            if let Some(edges) = parse_four(value, font_size, containing_width) {
                let widths = [edges.top, edges.right, edges.bottom, edges.left];
                for side in sides(name.trim_end_matches("-width")) {
                    set_border(style, side, Some(widths[side]), None);
                }
            }
        }
        "border-color" | "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            let colors: Vec<Color> = split_space(value).iter().filter_map(|t| parse_color(t)).collect();
            if !colors.is_empty() {
                let at = |i: usize| colors[match colors.len() { 1 => 0, 2 => i % 2, 3 => [0, 1, 2, 1][i], _ => i.min(3) }];
                for side in sides(name.trim_end_matches("-color")) {
                    set_border(style, side, None, Some(at(side)));
                }
            }
        }
        "border-style" | "border-top-style" | "border-right-style" | "border-bottom-style" | "border-left-style" => {
            if value == "none" || value == "hidden" {
                for side in sides(name.trim_end_matches("-style")) {
                    set_border(style, side, Some(0.0), None);
                }
            }
        }
        "fill" | "stroke" => {
            let paint = if value.eq_ignore_ascii_case("none") {
                Some(None)
            } else if value.eq_ignore_ascii_case("currentcolor") {
                Some(Some(style.color))
            } else {
                parse_color(value).map(Some)
            };
            if paint.is_some() {
                if name == "fill" { style.svg_fill = paint; } else { style.svg_stroke = paint; }
            }
        }
        "width" => {
            style.width = parse_length(value, font_size, containing_width);
            style.width_relative = value.contains('%') || value.contains("vw");
        }
        "height" => style.height = height_length(value, font_size),
        "line-height" => {
            style.line_height = if value == "normal" {
                LineHeight::Normal
            } else if let Ok(factor) = value.parse::<f32>() {
                LineHeight::Factor(factor.max(0.0))
            } else if let Some(px) = parse_length(value, font_size, font_size) {
                // em and % resolve against this element's font now, like CSS
                LineHeight::Px(px.max(0.0))
            } else {
                style.line_height
            };
        }
        "letter-spacing" => {
            style.letter_spacing = if value == "normal" { 0.0 } else { parse_length(value, font_size, font_size).unwrap_or(style.letter_spacing) };
        }
        "text-indent" => {
            if let Some(v) = parse_length(value, font_size, containing_width) {
                style.text_indent = v;
            }
        }
        "max-width" => style.max_width = if value == "none" { None } else { parse_length(value, font_size, containing_width) },
        "white-space" => {
            style.pre = value == "pre" || value == "pre-wrap" || value == "break-spaces";
            style.nowrap = value == "nowrap" || value == "pre";
        }
        "list-style-type" => {
            style.marker = Some(match value {
                "none" => "",
                "disc" => "•",
                "circle" => "◦",
                "square" => "▪",
                _ => "•",
            });
            style.list_marker = style.marker;
        }
        "list-style" => {
            if value.split_whitespace().any(|w| w == "none") {
                style.marker = Some("");
                style.list_marker = style.marker;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_document;

    #[test]
    fn root_rem_is_the_initial_size() {
        let doc = parse_document("<html style='font-size: 1.2rem'><body><p style='font-size: 2rem'>x</p></body></html>");
        let sheet = Stylesheet::default();
        let html = doc.find_first("html").unwrap();
        for _ in 0..3 {
            let s = compute(html, None, &sheet, &[&doc], 800.0);
            assert!((s.font_size - 19.2).abs() < 0.01, "{}", s.font_size);
        }
    }

    #[test]
    fn logical_properties_map_to_physical_ones() {
        let doc = parse_document("<div style='margin-inline: auto; padding-block: 4px 8px; max-inline-size: 300px; inset-inline-start: 5px'>x</div>");
        let sheet = Stylesheet::default();
        let d = doc.find_first("div").unwrap();
        let s = compute(d, Some(&default_style("body")), &sheet, &[&doc], 800.0);
        assert_eq!(s.margin_auto, (true, true));
        assert_eq!((s.padding.top, s.padding.bottom), (4.0, 8.0));
        assert_eq!(s.max_width, Some(300.0));
    }

    #[test]
    fn math_function_arguments_are_sums() {
        let v = parse_length("clamp(3rem, 1rem + 10px, 5rem)", 16.0, 100.0).unwrap();
        assert_eq!(v, 48.0);
        assert_eq!(parse_length("min(100% - 20px, 50px)", 16.0, 100.0), Some(50.0));
        assert_eq!(parse_length("max(10px, 100% - 20px)", 16.0, 100.0), Some(80.0));
    }

    #[test]
    fn font_shorthand_weight_is_not_a_size() {
        let doc = parse_document("<p style='font: 300 18px/1.2 Arial'>x</p><b style='font: 700 1.5rem Arial'>y</b>");
        let sheet = Stylesheet::default();
        let p = doc.find_first("p").unwrap();
        let s = compute(p, Some(&default_style("body")), &sheet, &[&doc], 800.0);
        assert_eq!(s.font_size, 18.0);
        let b = doc.find_first("b").unwrap();
        let s = compute(b, Some(&default_style("body")), &sheet, &[&doc], 800.0);
        assert_eq!(s.font_size, 24.0);
        assert!(s.bold);
    }

    #[test]
    fn grid_areas_require_rectangles_and_equal_row_lengths() {
        assert!(parse_grid_areas("\"a a\" \"b c\"").is_some());
        assert!(parse_grid_areas("\"a a\" \"a b\"").is_none());
        assert!(parse_grid_areas("\"a b\" \"a\"").is_none());
        assert!(parse_grid_areas("\". ..\"").is_some());
    }

    #[test]
    fn concurrent_page_workers_have_independent_viewports() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [(500.0, 400.0), (1600.0, 900.0)].into_iter().map(|(w, h)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                set_viewport(w, h);
                barrier.wait();
                assert_eq!(parse_length("100vw", 16.0, 0.0), Some(w));
                assert_eq!(parse_length("100vh", 16.0, 0.0), Some(h));
                assert_eq!(viewport_height(), h);
            })
        }).collect();
        for thread in threads { thread.join().unwrap(); }
    }

    #[test]
    fn rem_is_not_read_as_em_and_follows_the_root() {
        assert_eq!(parse_length("1rem", 30.0, 0.0), Some(16.0));
        assert_eq!(parse_length("2em", 30.0, 0.0), Some(60.0));
        ROOT_FONT.with(|r| r.set(10.0));
        assert_eq!(parse_length("1.5rem", 30.0, 0.0), Some(15.0));
        ROOT_FONT.with(|r| r.set(16.0));
    }

    #[test]
    fn defaults_for_common_tags() {
        assert_eq!(default_style("div").display, Display::Block);
        assert_eq!(default_style("a").underline, true);
        assert_eq!(default_style("b").bold, true);
        assert_eq!(default_style("script").display, Display::None);
        assert!(default_style("h1").font_size > default_style("h3").font_size);
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_length("0", 16.0, 500.0), Some(0.0));
        assert_eq!(parse_length("12px", 16.0, 500.0), Some(12.0));
        assert_eq!(parse_length("2em", 16.0, 500.0), Some(32.0));
        assert_eq!(parse_length("50%", 16.0, 500.0), Some(250.0));
        assert_eq!(parse_length("bogus", 16.0, 500.0), None);
    }

    #[test]
    fn four_sided_shorthand() {
        let edges = parse_four("1px 2px 3px 4px", 16.0, 100.0).unwrap();
        assert_eq!((edges.top, edges.right, edges.bottom, edges.left), (1.0, 2.0, 3.0, 4.0));
        let pair = parse_four("10px 20px", 16.0, 100.0).unwrap();
        assert_eq!((pair.top, pair.right), (10.0, 20.0));
        assert_eq!(pair.bottom, 10.0);
    }

    #[test]
    fn inheritance_and_cascade() {
        let sheet = Stylesheet::parse("div { color: rgb(1, 2, 3); font-size: 20px } span { color: red }");
        let doc = parse_document("<div><span>x</span></div>");
        let div = doc.find_first("div").unwrap();
        let span = doc.find_first("span").unwrap();
        let parent = compute(div, None, &sheet, &[&doc], 800.0);
        assert_eq!(parent.font_size, 20.0);
        let child = compute(span, Some(&parent), &sheet, &[&div, &doc], 800.0);
        assert_eq!(child.font_size, 20.0, "font-size inherits");
        assert_eq!(child.color, crate::css::Color::rgb(255, 0, 0), "own rule wins");
    }

    #[test]
    fn inline_style_beats_stylesheet() {
        let sheet = Stylesheet::parse("p { color: red }");
        let doc = parse_document(r#"<p style="color: #00ff00">x</p>"#);
        let p = doc.find_first("p").unwrap();
        let style = compute(p, None, &sheet, &[&doc], 800.0);
        assert_eq!(style.color, crate::css::Color::rgb(0, 255, 0));
    }
}
