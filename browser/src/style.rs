//! Computed style: user-agent defaults, inheritance, the cascade, shorthand boxes.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::css::{parse_color, parse_declarations, substitute_vars, Color, Declaration, Stylesheet};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
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
    pub position: Position,
    pub left: Option<f32>,
    pub top: Option<f32>,
    /// a transform that moves the box a whole box-length or more off its place
    pub moved_away: bool,
    /// grid-template-columns, resolved at layout (it needs the width)
    pub grid_columns: Option<String>,
    pub grid_span: GridSpan,
    /// custom properties in scope (inherited), last definition wins
    pub vars: Arc<Vec<(String, String)>>,
    /// font-family asks for a monospace face (inherited)
    pub monospace: bool,
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
            position: Position::Static,
            left: None,
            top: None,
            moved_away: false,
            grid_columns: None,
            grid_span: GridSpan::Auto,
            vars: Arc::new(Vec::new()),
            monospace: false,
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
        "head" | "style" | "script" | "title" | "meta" | "link" | "base" | "noscript" => {
            style.display = Display::None;
        }
        _ => {}
    }
    style
}

/// Lengths: px, em, rem, % (of the containing width), and bare numbers as px.
/// Viewport size in CSS px, for vw / vh (set by the layout).
static VIEWPORT_W: AtomicU32 = AtomicU32::new(0x4480_0000); // 1024.0
static VIEWPORT_H: AtomicU32 = AtomicU32::new(0x4448_0000); // 800.0

pub fn set_viewport(width: f32, height: f32) {
    VIEWPORT_W.store(width.to_bits(), Ordering::Relaxed);
    VIEWPORT_H.store(height.to_bits(), Ordering::Relaxed);
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
            let args: Vec<f32> = split_args(inner).iter().map(|a| parse_length(a, font_size, percent_base)).collect::<Option<Vec<f32>>>()?;
            return match kind {
                1 => args.into_iter().reduce(f32::min),
                2 => args.into_iter().reduce(f32::max),
                _ if args.len() == 3 => Some(args[1].clamp(args[0], args[2].max(args[0]))),
                _ => None,
            };
        }
    }
    let vw = f32::from_bits(VIEWPORT_W.load(Ordering::Relaxed));
    let vh = f32::from_bits(VIEWPORT_H.load(Ordering::Relaxed));
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
    if let Some(em) = value.strip_suffix("em") {
        let factor: f32 = em.trim().parse().ok()?;
        return Some(factor * font_size);
    }
    if let Some(rem) = value.strip_suffix("rem") {
        let factor: f32 = rem.trim().parse().ok()?;
        return Some(factor * 16.0);
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
    style.vars = parent.vars.clone();
    style.monospace = parent.monospace;
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
        style.vars = inherit.vars.clone();
        style.monospace = style.monospace || inherit.monospace;
        if style.family.is_none() {
            style.family = inherit.family.clone();
        }
        style.pre = style.pre || inherit.pre;
    }

    presentational_hints(node, &mut style, containing_width);

    let mut declarations: Vec<Declaration> = sheet.matching(node, ancestors);
    if let Some(inline) = node.attr("style") {
        declarations.extend(parse_declarations(inline));
    }
    // custom properties first (they inherit), then everything else with var() resolved
    let mut own_vars: Vec<(String, String)> = Vec::new();
    for declaration in declarations.iter().filter(|d| d.name.starts_with("--")) {
        own_vars.push((declaration.name.clone(), declaration.value.clone()));
    }
    if !own_vars.is_empty() {
        let mut all = (*style.vars).clone();
        for (name, value) in own_vars {
            // a custom property may itself use var()
            let value = substitute_vars(&value, &all).unwrap_or(value);
            all.retain(|(k, _)| *k != name);
            all.push((name, value));
        }
        style.vars = Arc::new(all);
    }
    for declaration in declarations.iter().filter(|d| !d.name.starts_with("--")) {
        if let Some(value) = substitute_vars(&declaration.value, &style.vars) {
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
            apply(&mut style, &declaration.name, &value, containing_width);
        }
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
        "text-align" => style.align = parent.align,
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

fn apply(style: &mut Style, name: &str, value: &str, containing_width: f32) {
    let value = value.trim();
    let font_size = style.font_size;
    match name {
        "display" => {
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
        "background" | "background-image" => {
            // a gradient layer, and/or a colour; url() images are not drawn
            if value == "none" || value == "transparent" {
                if name == "background" {
                    style.background = None;
                }
                style.gradient = None;
                return;
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
                if let Some(px) = keyword_size.or_else(|| if size.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') { parse_length(size, font_size, font_size) } else { None }) {
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
        "min-height" => style.min_height = parse_length(value, font_size, containing_width),
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
            if value.trim_end_matches('%').parse::<f32>().is_ok_and(|o| o <= 0.0) {
                style.hidden = true;
            }
        }
        "position" => {
            style.position = match value {
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "relative" | "sticky" | "-webkit-sticky" => Position::Relative,
                _ => Position::Static,
            }
        }
        "left" => style.left = parse_length(value, font_size, containing_width),
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
        "grid-column" => {
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
        "width" => style.width = parse_length(value, font_size, containing_width),
        "height" => style.height = parse_length(value, font_size, containing_width),
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
            })
        }
        "list-style" => {
            if value.contains("none") {
                style.marker = Some("");
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
