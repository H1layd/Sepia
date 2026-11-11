//! Computed style: user-agent defaults, inheritance, the cascade, shorthand boxes.

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
    pub line_height: f32,
    pub pre: bool,
    pub marker: Option<&'static str>,
    /// None = not specified (a cell then takes its row's, else middle).
    pub valign: Option<VAlign>,
    pub border_collapse: bool,
    pub flex: Flex,
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
            line_height: 1.35,
            pre: false,
            marker: None,
            valign: None,
            border_collapse: false,
            flex: Flex::default(),
        }
    }
}

/// The user-agent stylesheet: what a page gets for free.
pub fn default_style(tag: &str) -> Style {
    let mut style = Style::default();
    match tag {
        "html" | "body" | "div" | "section" | "article" | "header" | "footer" | "main"
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
        "code" | "kbd" | "samp" | "tt" => style.font_size = 14.0,
        "pre" => {
            style.display = Display::Block;
            style.pre = true;
            style.font_size = 14.0;
            style.margin = Edges { top: 13.0, right: 0.0, bottom: 13.0, left: 0.0 };
            style.background = Some(Color::rgb(245, 245, 245));
            style.padding = Edges::all(8.0);
        }
        "small" => style.font_size = 13.0,
        "big" => style.font_size = 20.0,
        "hr" => {
            style.display = Display::Block;
            style.border_width = 1.0;
            style.border_color = Color::GRAY;
            style.margin = Edges { top: 8.0, right: 0.0, bottom: 8.0, left: 0.0 };
        }
        "br" => style.display = Display::Inline,
        "input" | "button" | "select" | "textarea" => {
            style.display = Display::Inline;
            style.border_width = 1.0;
            style.border_color = Color::GRAY;
            style.background = Some(Color::WHITE);
            style.padding = Edges { top: 2.0, right: 4.0, bottom: 2.0, left: 4.0 };
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
pub fn parse_length(value: &str, font_size: f32, percent_base: f32) -> Option<f32> {
    let value = value.trim().to_lowercase();
    if value == "0" {
        return Some(0.0);
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
    if let Some(parent_style) = parent {
        let inherit = inherited(parent_style);
        if style.font_size == 16.0 {
            style.font_size = inherit.font_size;
        }
        if !style.bold {
            style.bold = inherit.bold;
        }
        if !style.italic {
            style.italic = inherit.italic;
        }
        style.color = inherit.color;
        style.align = inherit.align;
        style.line_height = inherit.line_height;
        style.pre = style.pre || inherit.pre;
    }

    presentational_hints(node, &mut style, containing_width);

    let mut declarations: Vec<Declaration> = sheet.matching(node, ancestors);
    if let Some(inline) = node.attr("style") {
        declarations.extend(parse_declarations(inline));
    }
    for declaration in declarations {
        apply(&mut style, &declaration.name, &declaration.value, containing_width);
    }
    style
}

/// Old HTML attributes that still style much of the web (HN is built on
/// them). They rank below any CSS, so they are applied first.
fn presentational_hints(node: &Node, style: &mut Style, containing_width: f32) {
    let tag = node.tag.as_str();
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
                "flex" | "inline-flex" => Display::Flex,
                "block" | "grid" | "inline-grid" => Display::Block,
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
        "background-color" | "background" => {
            if let Some(color) = parse_color(value) {
                style.background = Some(color);
            }
        }
        "font-size" => {
            if let Some(size) = parse_length(value, font_size, font_size) {
                style.font_size = size.max(4.0);
            }
        }
        "font-weight" => {
            style.bold = match value {
                "bold" | "bolder" => true,
                "normal" => false,
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
            if let Some(edges) = parse_four(value, font_size, containing_width) {
                style.margin = edges;
            }
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
            if let Some(v) = parse_length(value, font_size, containing_width) {
                match name {
                    "margin-top" => style.margin.top = v,
                    "margin-right" => style.margin.right = v,
                    "margin-bottom" => style.margin.bottom = v,
                    _ => style.margin.left = v,
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
        "border" | "border-top" | "border-bottom" | "border-left" | "border-right" => {
            for part in value.split_whitespace() {
                if let Some(color) = parse_color(part) {
                    style.border_color = color;
                } else if let Some(width) = parse_length(part, font_size, containing_width) {
                    style.border_width = width;
                }
            }
        }
        "border-width" => {
            if let Some(width) = parse_length(value, font_size, containing_width) {
                style.border_width = width;
            }
        }
        "border-color" => {
            if let Some(color) = parse_color(value) {
                style.border_color = color;
            }
        }
        "width" => style.width = parse_length(value, font_size, containing_width),
        "height" => style.height = parse_length(value, font_size, containing_width),
        "line-height" => {
            if let Some(height) = parse_length(value, font_size, font_size) {
                style.line_height = if value.ends_with("px") {
                    (height / font_size).max(1.0)
                } else if value.parse::<f32>().is_ok() && !value.ends_with('%') {
                    height.max(1.0)
                } else {
                    height.max(1.0)
                };
            }
        }
        "white-space" => style.pre = value == "pre" || value == "pre-wrap",
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
