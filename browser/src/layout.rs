//! Layout: block flow with inline text lines, measured with real font metrics.

use std::path::PathBuf;
use std::sync::Arc;

use fontdue::{Font, FontSettings};

use crate::css::{Color, Stylesheet};
use crate::dom::Node;
use crate::images::{Bitmap, ImageMap};
use crate::style::{compute, Align, Display, Edges, Gradient, Inner, LineHeight, Shadow, Style, TextTransform, VAlign};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

#[derive(Debug, Clone)]
pub enum Draw {
    Rect { rect: Rect, color: Color },
    Border { rect: Rect, width: f32, color: Color },
    Text {
        x: f32,
        baseline: f32,
        text: String,
        size: f32,
        color: Color,
        bold: bool,
        italic: bool,
        underline: bool,
        /// extra px after each character (CSS letter-spacing)
        letter_spacing: f32,
        /// the CSS font-family list the text is set in
        family: Option<Arc<str>>,
    },
    /// An `<img>`; `image` is None when it could not be fetched or decoded
    /// and paint shows a placeholder with the `alt` text.
    Image { rect: Rect, image: Option<Arc<Bitmap>>, alt: String },
    /// A box background: colour and/or linear gradient, rounded corners.
    Fill { rect: Rect, radius: f32, color: Option<Color>, gradient: Option<Gradient> },
    /// A box border, per side (top, right, bottom, left), rounded corners.
    Edges { rect: Rect, widths: Edges, colors: [Color; 4], radius: f32 },
    /// An outer box-shadow under `rect`.
    Shadow { rect: Rect, radius: f32, shadow: Shadow },
}

#[derive(Debug, Clone)]
pub struct LinkHit {
    pub rect: Rect,
    pub href: String,
    /// the `<a>` element (its `data-sepia-id`), when scripts run
    pub node: Option<usize>,
}

/// Multiply what a draw paints by `opacity` (CSS opacity on its box).
fn fade(draw: &mut Draw, opacity: f32) {
    let f = |c: &mut Color| c.a = (c.a as f32 * opacity).round() as u8;
    match draw {
        Draw::Rect { color, .. } | Draw::Border { color, .. } | Draw::Text { color, .. } => f(color),
        Draw::Fill { color, gradient, .. } => {
            if let Some(c) = color {
                f(c);
            }
            if let Some(g) = gradient {
                for stop in g.stops.iter_mut() {
                    f(&mut stop.0);
                }
            }
        }
        Draw::Edges { colors, .. } => colors.iter_mut().for_each(f),
        Draw::Shadow { shadow, .. } => f(&mut shadow.color),
        Draw::Image { .. } => {}
    }
}

/// Inside layout a link's href carries its element id after a NUL; split it.
fn link(rect: Rect, href: &str) -> LinkHit {
    match href.split_once('\0') {
        Some((h, id)) => LinkHit { rect, href: h.to_string(), node: id.parse().ok() },
        None => LinkHit { rect, href: href.to_string(), node: None },
    }
}

fn link_target(node: &Node, href: &str) -> String {
    match node.attr("data-sepia-id") {
        Some(id) => format!("{href}\0{id}"),
        None => href.to_string(),
    }
}

#[derive(Debug, Default, Clone)]
pub struct Page {
    pub draws: Vec<Draw>,
    pub links: Vec<LinkHit>,
    pub text: Vec<String>,
    pub height: f32,
    /// The canvas colour (the root's background, propagated as CSS does).
    pub background: Option<Color>,
    /// Right edge of the widest line box (inline content, atoms included):
    /// what measuring min/max-content reads.
    pub content_right: f32,
    /// Element boxes with their `data-sepia-id` (only when scripts run):
    /// what a click lands on.
    pub boxes: Vec<(Rect, usize)>,
    /// `id` (and `<a name>`) of block boxes with their top: where `#fragment` scrolls
    pub anchors: Vec<(String, f32)>,
}

pub struct Fonts {
    pub regular: Font,
    pub bold: Font,
    pub italic: Font,
    /// Faces for codepoints the styled face lacks (CJK, symbols...), loaded on first miss.
    pub fallback: crate::fonts::Fallbacks,
    /// Colour emoji (Apple sbix / Noto CBDT), loaded on first emoji.
    pub emoji: crate::fonts::LazyEmoji,
    /// Families pages brought with @font-face.
    pub web: crate::fonts::WebFonts,
    /// Local faces for `monospace` and `serif`.
    pub mono: crate::fonts::LocalFamily,
    pub serif: crate::fonts::LocalFamily,
    pub system: crate::fonts::LocalFamily,
}

impl Fonts {
    pub fn load() -> Fonts {
        let candidates: Vec<PathBuf> = std::env::var("SEPIA_FONT")
            .or_else(|_| std::env::var("VEIL_FONT"))
            .ok()
            .map(|value| value.split(':').map(PathBuf::from).collect())
            .unwrap_or_default();
        let mut paths = candidates;
        paths.extend([
            PathBuf::from("/System/Library/Fonts/Supplemental/Arial.ttf"),
            PathBuf::from("/System/Library/Fonts/Supplemental/Helvetica.ttf"),
            PathBuf::from("/System/Library/Fonts/SFNS.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
            PathBuf::from("/usr/share/fonts/TTF/DejaVuSans.ttf"),
            PathBuf::from(r"C:\Windows\Fonts\arial.ttf"),
        ]);
        let mut bold_paths = vec![
            PathBuf::from("/System/Library/Fonts/Supplemental/Arial Bold.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
            PathBuf::from(r"C:\Windows\Fonts\arialbd.ttf"),
        ];
        let mut italic_paths = vec![
            PathBuf::from("/System/Library/Fonts/Supplemental/Arial Italic.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf"),
            PathBuf::from(r"C:\Windows\Fonts\ariali.ttf"),
        ];

        let load_first = |paths: &mut Vec<PathBuf>| -> Option<Font> {
            for path in paths.iter() {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                        return Some(font);
                    }
                }
            }
            None
        };

        let regular = load_first(&mut paths).expect(
            "sepia-browser needs a TTF font: tried Arial, Helvetica, SFNS, DejaVu; \
             set SEPIA_FONT to pin one",
        );
        let bold = load_first(&mut bold_paths).unwrap_or_else(|| {
            Font::from_bytes(std::fs::read("/System/Library/Fonts/Supplemental/Arial.ttf")
                .unwrap_or_default(), FontSettings::default()).unwrap_or_else(|_| {
                Font::from_bytes(Vec::new(), FontSettings::default()).ok().unwrap()
            })
        });
        let italic = load_first(&mut italic_paths);
        let italic = match italic {
            Some(font) => font,
            None => Font::from_bytes(Vec::new(), FontSettings::default())
                .unwrap_or_else(|_| unreachable!()),
        };
        Fonts {
            regular,
            bold,
            italic,
            fallback: crate::fonts::Fallbacks::system(),
            emoji: crate::fonts::LazyEmoji::system(),
            web: crate::fonts::WebFonts::default(),
            mono: crate::fonts::LocalFamily::mono(),
            serif: crate::fonts::LocalFamily::serif(),
            system: crate::fonts::LocalFamily::system(),
        }
    }

    pub fn pick(&self, bold: bool, italic: bool) -> &Font {
        if bold && self.bold.units_per_em() > 0.0 {
            &self.bold
        } else if italic && self.italic.units_per_em() > 0.0 {
            &self.italic
        } else {
            &self.regular
        }
    }

    pub fn advance(&self, text: &str, size: f32, bold: bool, italic: bool) -> f32 {
        // Measure with exactly the faces paint will draw with, glyph by glyph,
        // so wrapping, underlines and link boxes match the pixels.
        let mut width = 0.0;
        self.for_each_glyph(text, bold, italic, |glyph| width += glyph.advance(size));
        width
    }

    pub fn ascent(&self, size: f32, bold: bool) -> f32 {
        let font = if bold { &self.bold } else { &self.regular };
        font.horizontal_line_metrics(size)
            .map(|m| m.ascent)
            .unwrap_or(size * 0.8)
    }

    pub fn descent(&self, size: f32, bold: bool) -> f32 {
        let font = if bold { &self.bold } else { &self.regular };
        font.horizontal_line_metrics(size)
            .map(|m| m.descent.abs())
            .unwrap_or(size * 0.2)
    }
}

struct InlineItem {
    text: String,
    style: Style,
    href: Option<String>,
    br: bool,
    image: bool,
    /// Set only when this element itself declared a background: a block's own
    /// background must never become a per-word patch painted over the line.
    own_background: Option<Color>,
    /// `<img>`: a replaced box on the line instead of text.
    img: Option<ImgBox>,
    /// the inline elements this piece sits in (their `data-sepia-id`s):
    /// each gets the piece's rectangle as part of its geometry
    owners: Vec<usize>,
}

#[derive(Clone)]
struct ImgBox {
    w: f32,
    h: f32,
    bitmap: Option<Arc<Bitmap>>,
    alt: String,
    /// An inline-block (button, input, badge…): its laid-out box at the
    /// origin, and the y of its baseline inside it.
    atom: Option<Arc<(Page, f32)>>,
}

pub struct Layout<'a> {
    pub fonts: &'a Fonts,
    pub sheet: &'a Stylesheet,
    pub viewport_width: f32,
    images: Option<&'a ImageMap>,
    /// Measuring min/max-content for table columns: lines start at the left
    /// edge whatever their alignment, so the extent of the draws is the width.
    measuring: bool,
    /// the opacity of each box being laid out, innermost last
    opacity_slots: Vec<f32>,
    /// inside a min/max-content measurement (at any depth): positioned
    /// boxes are out of flow and give their ancestors no size
    intrinsic: bool,
    /// text-indent waiting for the next line flushed (the block's first)
    pending_indent: f32,
    /// bottom margin of the block laid out last, for collapsing with what follows
    last_margin_bottom: f32,
    /// bottom margin of the last block child, escaping its parent
    trailing_margin: f32,
    /// the element whose background became the canvas (not painted again)
    canvas_node: Option<*const Node>,
    /// width of the line box being filled (inline-blocks shrink to fit it)
    flow_width: f32,
    /// a flex/grid item laid out as a block whatever its inline display
    blockify: Option<*const Node>,
    /// a flex row item laid out at the size the line gave it: its declared
    /// width (often 100%) yields to that size, margins included
    flex_main: Option<*const Node>,
    /// positioned ancestors (their padding boxes), innermost last: what
    /// `position: absolute` children are placed against
    containers: Vec<Container>,
    /// floats placed so far in this flow (page coordinates): lines beside them get shorter
    exclusions: Vec<Exclusion>,
    page: Page,
}

#[derive(Clone, Copy)]
struct Exclusion {
    top: f32,
    bottom: f32,
    /// the float's outer box, margins included
    x0: f32,
    x1: f32,
    right: bool,
}

/// The padding box of a positioned element while its children are laid out.
/// Its height is known only at the end, so absolutes anchored by `bottom`
/// are moved into place then.
struct Container {
    x: f32,
    y: f32,
    w: f32,
    /// (draws, links, boxes) ranges to move, and where their bottom edge goes: `bottom` and box height
    bottom_fixes: Vec<(std::ops::Range<usize>, std::ops::Range<usize>, std::ops::Range<usize>, f32, f32)>,
}

fn out_of_flow(style: &Style) -> bool {
    matches!(style.position, crate::style::Position::Absolute | crate::style::Position::Fixed) && style.display != Display::None
}

impl<'a> Layout<'a> {
    pub fn new(fonts: &'a Fonts, sheet: &'a Stylesheet, viewport_width: f32) -> Layout<'a> {
        crate::style::set_viewport(viewport_width, sheet.viewport_height);
        Layout { fonts, sheet, viewport_width, images: None, measuring: false, intrinsic: false, opacity_slots: Vec::new(), pending_indent: 0.0, last_margin_bottom: 0.0, trailing_margin: 0.0, canvas_node: None, flow_width: viewport_width, blockify: None, flex_main: None, containers: Vec::new(), exclusions: Vec::new(), page: Page::default() }
    }

    /// Decoded images of the document, keyed by `src` as written.
    pub fn with_images(mut self, images: &'a ImageMap) -> Layout<'a> {
        self.images = Some(images);
        self
    }

    /// Advance of `text` in `style`, letter-spacing included.
    fn text_width(&self, text: &str, style: &Style) -> f32 {
        let base = self.fonts.advance_in(text, style.font_size, style.bold, style.italic, style.family.as_deref());
        if style.letter_spacing == 0.0 {
            base
        } else {
            base + style.letter_spacing * text.chars().count() as f32
        }
    }

    /// Width a line entry occupies: the image box, or the text advance.
    fn item_width(&self, text: &str, item: &InlineItem) -> f32 {
        match &item.img {
            Some(img) => img.w,
            None => self.text_width(text, &item.style),
        }
    }

    /// Lay out a document (or a fragment): `<html>`'s and `<body>`'s styles
    /// apply, the body's margin (8px by default) collapses with its first and
    /// last child, and the root background becomes the canvas, as in CSS.
    pub fn build(mut self, root: &Node) -> Page {
        let _matching_scope = crate::css::cache_layout_matches();
        let html = if root.tag == "html" { Some(root) } else { root.find_first("html") };
        let synthetic;
        let body: &Node = match root.find_first("body") {
            Some(body) => body,
            None if root.tag == "body" => root,
            None => {
                // a fragment: wrap it in a body so it gets the body's defaults
                let children = if root.tag == "#document" || root.tag == "html" { root.children.clone() } else { vec![root.clone()] };
                synthetic = Node { tag: "body".into(), attrs: Vec::new(), children, text: None };
                &synthetic
            }
        };
        let html_style = html.map(|h| compute(h, None, self.sheet, &[], self.viewport_width));
        let ancestors: Vec<&Node> = html.into_iter().collect();
        let body_style = compute(body, html_style.as_ref(), self.sheet, &ancestors, self.viewport_width);
        // the canvas takes the root's background, else the body's
        let canvas = match &html_style {
            Some(h) if h.background.is_some() || h.gradient.is_some() => (h.background, h.gradient.clone()),
            _ => {
                if body_style.background.is_some() || body_style.gradient.is_some() {
                    self.canvas_node = Some(body as *const Node);
                }
                (body_style.background, body_style.gradient.clone())
            }
        };
        let height = self.layout_block(body, &ancestors, html_style.as_ref(), 0.0, 0.0, self.viewport_width);
        self.page.height = height.max(1.0);
        // a dark used scheme on the root darkens the canvas, as browsers do
        let dark_root = html_style.as_ref().map_or(body_style.dark, |h| h.dark);
        self.page.background = canvas.0.or_else(|| dark_root.then(|| Color::rgb(18, 18, 18)));
        if let Some(gradient) = canvas.1 {
            let rect = Rect { x: 0.0, y: 0.0, w: self.viewport_width, h: self.page.height };
            self.page.draws.insert(0, Draw::Fill { rect, radius: 0.0, color: None, gradient: Some(gradient) });
        }
        self.page
    }

    pub fn title(document: &Node) -> String {
        document
            .find_first("title")
            .map(|node| collapse_ws(&node.text_content()).trim().to_string())
            .unwrap_or_default()
    }

    /// `parent` is the computed style of the element this block sits in, so
    /// inherited properties (colour, font, alignment) come from what the
    /// cascade actually produced there; without it, the parent tag's defaults.
    fn layout_block(
        &mut self,
        node: &Node,
        ancestors: &[&Node],
        parent: Option<&Style>,
        x: f32,
        y: f32,
        containing_width: f32,
    ) -> f32 {
        // opacity applies to the whole box with what is inside it: what it drew
        // gets the factor (nested ones multiply, as group opacity does)
        let first_draw = self.page.draws.len();
        self.opacity_slots.push(1.0);
        let height = self.layout_block_inner(node, ancestors, parent, x, y, containing_width);
        let opacity = self.opacity_slots.pop().unwrap_or(1.0);
        if opacity < 0.999 {
            for draw in &mut self.page.draws[first_draw..] {
                fade(draw, opacity);
            }
        }
        height
    }

    fn layout_block_inner(
        &mut self,
        node: &Node,
        ancestors: &[&Node],
        parent: Option<&Style>,
        x: f32,
        y: f32,
        containing_width: f32,
    ) -> f32 {
        if node.is_text() {
            let text = node.text.clone().unwrap_or_default();
            if text.trim().is_empty() {
                return 0.0;
            }
            // Stray text at block level: lay it out as an inline line.
            self.last_margin_bottom = 0.0;
            let style = parent.map(crate::style::inherited).unwrap_or_default();
            let items = vec![InlineItem {
                text: collapse_ws(&text),
                style,
                href: None,
                br: false,
                image: false,
                own_background: None,
                img: None,
                owners: Vec::new(),
            }];
            return self.layout_inline(&items, x, y, containing_width);
        }

        let parent_style = parent.cloned().or_else(|| ancestors.last().map(|n| default_for(n)));
        let mut style = compute(node, parent_style.as_ref(), self.sheet, ancestors, containing_width);
        if let Some(slot) = self.opacity_slots.last_mut() {
            *slot = style.opacity;
        }
        // a form control as a flex/grid item takes the size its line gives it
        if self.blockify == Some(node as *const Node) && !self.measuring && matches!(node.tag.as_str(), "input" | "select" | "textarea") && style.width.is_none() {
            style.width = Some((containing_width - style.margin.horizontal()).max(1.0));
            style.border_box = true;
        }
        if self.blockify == Some(node as *const Node) && matches!(style.display, Display::Inline | Display::InlineBlock) && !matches!(node.tag.as_str(), "img" | "svg" | "input" | "select" | "textarea") {
            style.display = match (style.display, style.inner) {
                (Display::InlineBlock, Inner::Flex) => Display::Flex,
                (Display::InlineBlock, Inner::Grid) => Display::Grid,
                _ => Display::Block,
            };
        }
        if style.display == Display::None {
            return 0.0;
        }
        if self.flex_main == Some(node as *const Node) && !self.measuring {
            style.width = Some((containing_width - style.margin.horizontal()).max(1.0));
            style.border_box = true;
            style.min_width = None;
            style.max_width = None;
        }

        if style.display == Display::Table {
            let outer_align = parent_style.as_ref().map(|p| p.align).unwrap_or(Align::Left);
            self.last_margin_bottom = style.margin.bottom.max(0.0);
            return self.layout_table(node, ancestors, &style, outer_align, x, y, containing_width);
        }

        let mut ancestors2: Vec<&Node> = ancestors.to_vec();
        ancestors2.push(node);

        // Replaced elements (images, inline SVG) drawn as blocks are a line
        // holding their box: they have no children to lay out.
        // A blockified control must keep the size resolved by flex/grid.
        // collect_inline would compute its style again and lose that width.
        if self.blockify == Some(node as *const Node) && matches!(node.tag.as_str(), "input" | "select" | "textarea") {
            let saved_flow = std::mem::replace(&mut self.flow_width, containing_width);
            let atom = self.make_atom(node, &ancestors2, &style);
            self.flow_width = saved_flow;
            if let Some((page, _, _, height)) = atom {
                self.append_page(page, x, y);
                return height;
            }
            return 0.0;
        }
        let replaced = matches!(node.tag.as_str(), "img" | "svg");
        if replaced || matches!(style.display, Display::Inline | Display::InlineBlock) {
            // Inline elements at block level are laid out as their own line.
            self.last_margin_bottom = 0.0;
            let mut items: Vec<InlineItem> = Vec::new();
            self.collect_inline(node, &ancestors2, &style, None, &mut items);
            return self.layout_inline(&items, x, y, containing_width);
        }

        self.layout_box(node, &ancestors2, &style, x, y, containing_width)
    }

    /// The block box of `node` with its style already computed: margins,
    /// border (per side), padding, background, shadow, sizing (width,
    /// min/max-width, height, min-height, box-sizing), then its children.
    /// `ancestors` ends with `node`. Returns the height used, margins included.
    fn layout_box(&mut self, node: &Node, ancestors2: &[&Node], style: &Style, x: f32, y: f32, containing_width: f32) -> f32 {
        let b = style.border;
        let pad = style.padding;
        let margin = style.margin;
        let chrome_w = pad.horizontal() + b.left + b.right;
        let chrome_h = pad.vertical() + b.top + b.bottom;
        // width / max-width size the box; auto margins share what is left
        let available = containing_width - margin.horizontal();
        let border_box = |w: f32| if style.border_box { w } else { w + chrome_w };
        let mut box_width = style.width.map(border_box).unwrap_or(available);
        if let Some(max_width) = style.max_width {
            box_width = box_width.min(border_box(max_width));
        }
        if let Some(min_width) = style.min_width {
            box_width = box_width.max(border_box(min_width));
        }
        let content_width = (box_width - chrome_w).max(1.0);
        let outer_width = content_width + chrome_w;
        let free = (available - outer_width).max(0.0);
        let auto_shift = match style.margin_auto {
            (true, true) => free / 2.0,
            (true, false) => free,
            _ => 0.0,
        };
        let box_x = x + margin.left + auto_shift;
        let box_y = y + margin.top;
        let inner_x = box_x + b.left + pad.left;
        let inner_y = box_y + b.top + pad.top;

        // shadow and background go under the children; their height is known at the end
        // while measuring content, a box without a width of its own fills the
        // measuring space (capped only by max-width): its background and
        // border are not content and must not read as width
        let measuring_auto = self.intrinsic && style.width.is_none();
        let paints = !style.hidden && !style.transparent && !measuring_auto;
        let paints_background = paints && self.canvas_node != Some(node as *const Node);
        let zero = Rect { x: box_x, y: box_y, w: 0.0, h: 0.0 };
        let shadow_index = match (paints_background, style.shadow) {
            (true, Some(shadow)) => {
                self.page.draws.push(Draw::Shadow { rect: zero, radius: style.radius, shadow });
                Some(self.page.draws.len() - 1)
            }
            _ => None,
        };
        let fill_index = if paints_background && (style.background.is_some() || style.gradient.is_some()) {
            self.page.draws.push(Draw::Fill { rect: zero, radius: style.radius, color: style.background, gradient: style.gradient.clone() });
            Some(self.page.draws.len() - 1)
        } else {
            None
        };
        // a background image goes over the colour, under the content; placed at the end
        let bg_bitmap = if paints_background { style.bg_image.as_deref().and_then(|src| self.images.and_then(|m| m.get(src))).cloned() } else { None };
        // a mask: the image's alpha in the background colour
        let bg_bitmap = match (bg_bitmap, style.mask_tint) {
            (Some(b), Some(c)) => Some(tinted_mask(&b, c)),
            (b, _) => b,
        };
        let bg_index = bg_bitmap.as_ref().map(|_| {
            self.page.draws.push(blank_draw());
            self.page.draws.len() - 1
        });

        let positioned = style.position != crate::style::Position::Static;
        if positioned {
            self.containers.push(Container { x: box_x + b.left, y: box_y + b.top, w: outer_width - b.left - b.right, bottom_fixes: Vec::new() });
        }
        let first_draw = shadow_index.or(fill_index).unwrap_or(self.page.draws.len());
        let (first_link, first_box) = (self.page.links.len(), self.page.boxes.len());
        let mut cursor = inner_y;
        // list marker
        if style.display == Display::ListItem && !style.hidden && !style.transparent {
            if let Some(marker) = style.marker {
                if !marker.is_empty() {
                    let baseline = cursor + self.fonts.ascent(style.font_size, style.bold);
                    self.page.draws.push(Draw::Text {
                        x: inner_x - 18.0,
                        baseline,
                        text: marker.to_string(),
                        size: style.font_size,
                        color: style.color,
                        bold: false,
                        italic: false,
                        underline: false,
                        letter_spacing: 0.0,
                        family: None,
                    });
                }
            }
        }

        // Margins collapse through a block with no border or padding on that side.
        let open_top = b.top == 0.0 && pad.top == 0.0;
        let open_bottom = b.bottom == 0.0 && pad.bottom == 0.0 && style.height.is_none() && style.min_height.is_none();
        let flex = style.display == Display::Flex || (style.display == Display::InlineBlock && style.inner == Inner::Flex);
        let grid = style.display == Display::Grid || (style.display == Display::InlineBlock && style.inner == Inner::Grid);
        let mut escaping = 0.0;
        let first_child_draw = self.page.draws.len();
        let first_child_link = self.page.links.len();
        cursor += if flex {
            self.layout_flex(node, ancestors2, style, inner_x, cursor, content_width)
        } else if grid {
            self.layout_grid(node, ancestors2, style, inner_x, cursor, content_width)
        } else {
            let collapse_top = if open_top { margin.top.max(0.0) } else { 0.0 };
            let used = self.layout_children(node, ancestors2, style, inner_x, cursor, content_width, collapse_top);
            if open_bottom {
                // the last child's bottom margin leaves the box and joins ours
                escaping = self.trailing_margin;
            }
            used - escaping
        };
        let margin_bottom = margin.bottom.max(0.0).max(escaping);

        let mut content_height = cursor - inner_y;
        let content_box = |h: f32| if style.border_box { (h - chrome_h).max(0.0) } else { h };
        if let Some(height) = style.height {
            // a clipped box keeps its height; others grow to hold what is in them
            content_height = if style.clip_y { content_box(height) } else { content_height.max(content_box(height)) };
        }
        if let Some(min_height) = style.min_height {
            content_height = content_height.max(content_box(min_height));
        }
        // a button centres its contents in a box taller than they are
        let natural = cursor - inner_y;
        if node.tag == "button" && content_height > natural + 0.5 {
            let dy = (content_height - natural) / 2.0;
            for draw in &mut self.page.draws[first_child_draw..] {
                translate(draw, 0.0, dy);
            }
            for link in &mut self.page.links[first_child_link..] {
                link.rect.y += dy;
            }
        }
        let total = content_height + chrome_h;
        let rect = Rect { x: box_x, y: box_y, w: outer_width, h: total };
        for index in [shadow_index, fill_index].into_iter().flatten() {
            if let Draw::Shadow { rect: r, .. } | Draw::Fill { rect: r, .. } = &mut self.page.draws[index] {
                *r = rect;
            }
        }
        if let (Some(index), Some(bitmap)) = (bg_index, bg_bitmap) {
            // painted inside the border (the padding box)
            let area = Rect { x: rect.x + b.left, y: rect.y + b.top, w: rect.w - b.left - b.right, h: rect.h - b.top - b.bottom };
            if let Some(draw) = background_draw(&bitmap, area, style) {
                self.page.draws[index] = draw;
            }
        }
        if paints && (b.top > 0.0 || b.right > 0.0 || b.bottom > 0.0 || b.left > 0.0) {
            self.page.draws.push(Draw::Edges { rect, widths: b, colors: style.border_colors, radius: style.radius });
        }
        // a box with a width of its own is that wide for whoever measures content
        // (a percentage of the huge measuring width is not a size: left out)
        if style.width.is_some() && !style.width_relative && !style.hidden && rect.x + rect.w < 50_000.0 {
            self.page.content_right = self.page.content_right.max(rect.x + rect.w);
        }
        if let Some(id) = node.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok()) {
            self.page.boxes.push((rect, id));
        }
        // a block-level anchor (block, flex, grid: cards, tiles, wrappers) is
        // clickable over its whole box, as in any browser
        if node.tag == "a" && rect.h > 0.0 && !style.hidden {
            if let Some(href) = node.attr("href").map(|h| link_target(node, h)).filter(|h| !h.is_empty()) {
                self.page.links.push(link(rect, &href));
            }
        }
        for name in [node.attr("id"), (node.tag == "a").then(|| node.attr("name")).flatten()].into_iter().flatten() {
            if !name.is_empty() {
                self.page.anchors.push((name.to_string(), box_y));
            }
        }
        if style.clip_x || style.clip_y {
            self.clip(first_child_draw, first_child_link, rect, style.clip_x, style.clip_y);
        }
        if positioned {
            if let Some(container) = self.containers.pop() {
                let inner_h = total - b.top - b.bottom;
                for (draws, links, boxes, bottom, h) in container.bottom_fixes {
                    self.shift(draws, links, boxes, 0.0, inner_h - bottom - h);
                }
            }
        }
        // position: relative moves the box and what is in it, leaving its place in the flow
        if style.position == crate::style::Position::Relative {
            let dx = style.left.or(style.right.map(|r| -r)).unwrap_or(0.0);
            let dy = style.top.or(style.bottom.map(|b| -b)).unwrap_or(0.0);
            if dx != 0.0 || dy != 0.0 {
                let (d, l, bx) = (self.page.draws.len(), self.page.links.len(), self.page.boxes.len());
                self.shift(first_draw..d, first_link..l, first_box..bx, dx, dy);
            }
        }
        self.last_margin_bottom = margin_bottom;
        total + margin.top + margin_bottom
    }

    /// Lay out the children of `node` (whose own box is already placed) in a
    /// column of `width` starting at (x, y); returns the height used.
    /// `ancestors` ends with `node` itself.
    /// Adjacent block margins collapse to the larger of the two; the first
    /// block child also collapses with `collapse_top` (the parent's own top
    /// margin, when nothing separates them). The bottom margin of a trailing
    /// block child is left in `self.trailing_margin` for the parent.
    #[allow(clippy::too_many_arguments)]
    fn layout_children(&mut self, node: &Node, ancestors: &[&Node], style: &Style, x: f32, y: f32, width: f32, collapse_top: f32) -> f32 {
        let saved_flow = std::mem::replace(&mut self.flow_width, width);
        let mut cursor = y;
        let saved_exclusions = self.exclusions.len();
        let mut previous_margin: Option<f32> = Some(collapse_top);
        let mut last_was_block = false;
        let mut items: Vec<InlineItem> = Vec::new();
        let current_href: Option<String> =
            if node.tag == "a" { node.attr("href").map(|h| h.to_string()) } else { None };
        for child in &node.children {
            let child_style = child.text.is_none().then(|| compute(child, Some(style), self.sheet, ancestors, width));
            if let Some(cs) = child_style.as_ref().filter(|cs| out_of_flow(cs)) {
                // its static position: where the next line would start
                let pending = if items.is_empty() { 0.0 } else { self.measure_inline_height(&items, width) };
                self.place_out_of_flow(child, ancestors, cs, x, cursor + pending);
                continue;
            }
            if let Some(cs) = child_style.as_ref().filter(|cs| cs.float.is_some() && cs.display != Display::None && !matches!(child.tag.as_str(), "img" | "svg")) {
                if !items.is_empty() {
                    let used = self.layout_inline(&items, x, cursor, width);
                    items.clear();
                    if used > 0.0 {
                        cursor += used;
                    }
                }
                let mut with_node: Vec<&Node> = ancestors.to_vec();
                with_node.push(child);
                let mut fs = cs.clone();
                if fs.display == Display::Inline {
                    fs.display = Display::InlineBlock;
                }
                let saved = std::mem::replace(&mut self.flow_width, width);
                let atom = self.atom_of(child, &with_node, &fs);
                self.flow_width = saved;
                if let Some((sub, w, h)) = atom {
                    // the highest place with room for it, beside floats already there
                    let mut top = cursor;
                    let (mut lx, mut lw) = (x, width);
                    for _ in 0..32 {
                        let (bx, bw, next) = self.band(top, h.max(1.0), x, width);
                        (lx, lw) = (bx, bw);
                        match next {
                            Some(below) if bw + 0.5 < w => top = below,
                            _ => break,
                        }
                    }
                    let right = cs.float == Some(Align::Right);
                    let fx = if right { lx + lw - w } else { lx };
                    self.append_page(sub, fx, top);
                    self.exclusions.push(Exclusion { top, bottom: top + h, x0: fx, x1: fx + w, right });
                }
                previous_margin = None;
                continue;
            }
            if child_style.as_ref().is_some_and(|cs| cs.clear) {
                if !items.is_empty() {
                    let used = self.layout_inline(&items, x, cursor, width);
                    items.clear();
                    cursor += used;
                }
                let lowest = self.exclusions.iter().map(|e| e.bottom).fold(cursor, f32::max);
                if lowest > cursor {
                    cursor = lowest;
                    previous_margin = None;
                }
            }
            let child_is_block = child_style.as_ref().is_some_and(|s| !matches!(s.display, Display::Inline | Display::InlineBlock | Display::None));
            if child_is_block {
                if !items.is_empty() {
                    let used = self.layout_inline(&items, x, cursor, width);
                    items.clear();
                    // whitespace between tags draws nothing and must not stop margins collapsing
                    if used > 0.0 {
                        cursor += used;
                        previous_margin = None;
                    }
                }
                let top = child_style.as_ref().map(|s| s.margin.top.max(0.0)).unwrap_or(0.0);
                cursor -= previous_margin.map(|m| m.min(top)).unwrap_or(0.0);
                cursor += self.layout_block(child, ancestors, Some(style), x, cursor, width);
                previous_margin = Some(self.last_margin_bottom);
                last_was_block = true;
            } else if child_style.as_ref().is_some_and(|s| s.display == Display::None) {
                continue;
            } else {
                self.collect_inline(child, ancestors, style, current_href.clone(), &mut items);
            }
        }
        if !items.is_empty() {
            let used = self.layout_inline(&items, x, cursor, width);
            if used > 0.0 {
                cursor += used;
                last_was_block = false;
            }
        }
        // the parent grows to hold its own floats (as if clearfixed); they stop here
        for e in self.exclusions.drain(saved_exclusions..) {
            cursor = cursor.max(e.bottom);
        }
        self.trailing_margin = if last_was_block { previous_margin.unwrap_or(0.0) } else { 0.0 };
        self.flow_width = saved_flow;
        cursor - y
    }

    fn collect_inline(
        &mut self,
        node: &Node,
        ancestors: &[&Node],
        parent_style: &Style,
        href: Option<String>,
        items: &mut Vec<InlineItem>,
    ) {
        if let Some(text) = &node.text {
            let collapsed = if parent_style.pre { text.clone() } else { collapse_ws(text) };
            let collapsed = match parent_style.text_transform {
                TextTransform::None => collapsed,
                TextTransform::Upper => collapsed.to_uppercase(),
                TextTransform::Lower => collapsed.to_lowercase(),
                TextTransform::Capitalize => {
                    let mut out = String::with_capacity(collapsed.len());
                    let mut start = true;
                    for c in collapsed.chars() {
                        if start && c.is_alphabetic() {
                            out.extend(c.to_uppercase());
                            start = false;
                        } else {
                            out.push(c);
                            start = c.is_whitespace();
                        }
                    }
                    out
                }
            };
            if !collapsed.is_empty() {
                items.push(InlineItem {
                    text: collapsed,
                    style: parent_style.clone(),
                    href: href.clone(),
                    br: false,
                    image: false,
                    own_background: None,
                    img: None,
                    owners: Vec::new(),
                });
            }
            return;
        }
        // percentages are of the line box being filled, not the window
        let style = compute(node, Some(parent_style), self.sheet, ancestors, self.flow_width);
        if style.display == Display::None {
            return;
        }
        if out_of_flow(&style) {
            // placed against its container; the static position (mid-line) is not known here
            let (x, y) = self.containers.last().map(|c| (c.x, c.y)).unwrap_or((0.0, 0.0));
            self.place_out_of_flow(node, ancestors, &style, x, y);
            return;
        }
        // img and svg are replaced elements whatever their display says
        // form controls are inline-blocks even when a site says `display: inline`
        let control = matches!(node.tag.as_str(), "button" | "input" | "select" | "textarea");
        if (style.display == Display::InlineBlock || control) && !matches!(node.tag.as_str(), "img" | "svg") {
            let mut node_ancestors: Vec<&Node> = ancestors.to_vec();
            node_ancestors.push(node);
            if let Some((page, baseline, w, h)) = self.make_atom(node, &node_ancestors, &style) {
                items.push(InlineItem {
                    text: String::new(),
                    style,
                    href: href.clone().or_else(|| if node.tag == "a" { node.attr("href").map(|h| link_target(node, h)) } else { None }),
                    br: false,
                    image: false,
                    own_background: None,
                    img: Some(ImgBox { w, h, bitmap: None, alt: String::new(), atom: Some(Arc::new((page, baseline))) }),
                    owners: Vec::new(),
                });
            }
            return;
        }
        match node.tag.as_str() {
            "svg" => {
                // inline SVG: drawn in the surrounding text colour (currentColor)
                let (nw, nh) = crate::svg::natural_size(node);
                let attr_px = |name: &str| node.attr(name).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).filter(|v| *v > 0.0);
                let w = style.width.or_else(|| attr_px("width"));
                let h = style.height.or_else(|| attr_px("height"));
                let (w, h) = match (w, h) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, w * nh / nw.max(0.01)),
                    (None, Some(h)) => (h * nw / nh.max(0.01), h),
                    (None, None) => (nw, nh),
                };
                if w < 0.5 || h < 0.5 {
                    return;
                }
                let scale = 3.0f32.min(1024.0 / w.max(h));
                let mut bitmap = crate::svg::render_node_styled(node, (w * scale).round() as usize, (h * scale).round() as usize, style.color, style.svg_fill, style.svg_stroke);
                bitmap.natural = Some((w, h));
                items.push(InlineItem {
                    text: String::new(),
                    style,
                    href: href.clone(),
                    br: false,
                    image: false,
                    own_background: None,
                    img: Some(ImgBox { w, h, bitmap: Some(Arc::new(bitmap)), alt: String::new(), atom: None }),
                    owners: Vec::new(),
                });
                return;
            }
            "br" => {
                items.push(InlineItem {
                    text: String::new(),
                    style,
                    href: None,
                    br: true,
                    image: false,
                    own_background: None,
                    img: None,
                    owners: Vec::new(),
                });
                return;
            }
            "img" => {
                let alt = node.attr("alt").unwrap_or("").trim().to_string();
                let bitmap = node
                    .attr("src")
                    .and_then(|src| self.images.and_then(|map| map.get(src)))
                    .cloned();
                match image_size(node, &style, bitmap.as_deref()) {
                    Some((w, h)) => items.push(InlineItem {
                        text: String::new(),
                        style,
                        href: href.clone(),
                        br: false,
                        image: false,
                        own_background: None,
                        img: Some(ImgBox { w, h, bitmap, alt, atom: None }),
                        owners: Vec::new(),
                    }),
                    // Nothing to size the box by: the alt text stands in, as plain text.
                    None if !alt.is_empty() => items.push(InlineItem {
                        text: alt,
                        style,
                        href: href.clone(),
                        br: false,
                        image: false,
                        own_background: None,
                        img: None,
                        owners: Vec::new(),
                    }),
                    None => {}
                }
                return;
            }
            "input" => {
                let placeholder = node
                    .attr("aria-label")
                    .or_else(|| node.attr("placeholder"))
                    .or_else(|| node.attr("value"))
                    .unwrap_or("");
                let width = node.attr("size").and_then(|s| s.parse::<f32>().ok()).unwrap_or(16.0) * 8.0;
                items.push(InlineItem {
                    text: format!("[{}]", if placeholder.is_empty() { "input".into() } else { placeholder.to_string() }),
                    style,
                    href: None,
                    br: false,
                    image: true,
                    own_background: None,
                    img: None,
                    owners: Vec::new(),
                });
                let _ = width;
                return;
            }
            "button" => {
                let label = node.text_content();
                items.push(InlineItem {
                    text: format!("[{}]", label.trim()),
                    style,
                    href: href.clone(),
                    br: false,
                    image: true,
                    own_background: None,
                    img: None,
                    owners: Vec::new(),
                });
                return;
            }
            _ => {}
        }
        let mut child_href = href;
        if node.tag == "a" {
            if let Some(target) = node.attr("href") {
                child_href = Some(link_target(node, target));
            }
        }
        let own_background = style.background;
        let mut child_ancestors: Vec<&Node> = ancestors.to_vec();
        child_ancestors.push(node);
        let first_item = items.len();
        for child in &node.children {
            self.collect_inline_styled(
                child,
                &child_ancestors,
                &style,
                child_href.clone(),
                items,
                own_background,
            );
        }
        if let Some(id) = node.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok()) {
            for item in &mut items[first_item..] {
                item.owners.push(id);
            }
        }
    }

    fn collect_inline_styled(
        &mut self,
        node: &Node,
        ancestors: &[&Node],
        parent_style: &Style,
        href: Option<String>,
        items: &mut Vec<InlineItem>,
        own_background: Option<Color>,
    ) {
        let before = items.len();
        self.collect_inline(node, ancestors, parent_style, href, items);
        if let Some(background) = own_background {
            for item in items.iter_mut().skip(before) {
                if item.own_background.is_none() {
                    item.own_background = Some(background);
                }
            }
        }
    }

    fn layout_inline(
        &mut self,
        items: &[InlineItem],
        x: f32,
        y: f32,
        width: f32,
    ) -> f32 {
        if items.is_empty() {
            return 0.0;
        }
        // lines beside floats are shorter: each line asks where it may go
        let line_guess = items.first().map(|i| i.style.font_size * 1.3).unwrap_or(20.0);
        let (mut cursor_y, mut lx, mut lw) = self.line_slot(y, x, width, line_guess);
        let mut line: Vec<(String, InlineItem)> = Vec::new();
        let mut line_width = 0.0f32;
        // text-indent: the first line of the block starts further right
        let mut indent = items.first().map(|i| i.style.text_indent).unwrap_or(0.0);
        self.pending_indent = indent;
        let mut max_size = 0.0f32;
        let mut line_done = false;
        // Whitespace is a property of the boundary between two runs, so it has
        // to survive element transitions: `</b>, <i>` keeps one space.
        let mut need_space = false;

        for item in items {
            if item.br {
                line_done = true;
            } else if item.img.is_some() {
                // Replaced box: shrink to the container width, keeping the aspect ratio.
                let mut boxed = item.clone_shallow();
                let w = match boxed.img.as_mut() {
                    Some(img) => {
                        if img.atom.is_none() && img.w > width && width > 0.0 {
                            img.h *= width / img.w;
                            img.w = width;
                        }
                        img.w
                    }
                    None => 0.0,
                };
                let space = if need_space && line_width > 0.0 {
                    self.text_width(" ", &item.style)
                } else {
                    0.0
                };
                if line_width + space + w > lw - indent && line_width > 0.0 && !item.style.nowrap {
                    self.flush_line(&mut line, lx, &mut cursor_y, lw, max_size);
                    (cursor_y, lx, lw) = self.line_slot(cursor_y, x, width, line_guess);
                    indent = 0.0;
                    line.clear();
                    line_width = 0.0;
                    max_size = 0.0;
                } else if space > 0.0 {
                    // the whitespace before the image, as a text run of its own
                    let mut gap = item.clone_shallow();
                    gap.img = None;
                    line.push((" ".to_string(), gap));
                    line_width += space;
                }
                max_size = max_size.max(item.style.font_size);
                line_width += w;
                line.push((String::new(), boxed));
                need_space = false;
            } else if item.image {
                let advance = self.text_width(&item.text, &item.style);
                if line_width + advance > lw - indent && line_width > 0.0 && !item.style.nowrap {
                    self.flush_line(&mut line, lx, &mut cursor_y, lw, max_size);
                    (cursor_y, lx, lw) = self.line_slot(cursor_y, x, width, line_guess);
                    indent = 0.0;
                    line.clear();
                    line_width = 0.0;
                    max_size = 0.0;
                }
                max_size = max_size.max(item.style.font_size);
                line_width += advance;
                line.push((item.text.clone(), item.clone_shallow()));
            } else {
                let words = split_words(&item.text, item.style.pre);
                for (word, trailing_space) in words {
                    if word.is_empty() {
                        // A text node that opens with whitespace: mark the
                        // boundary so the next word gets its leading space.
                        need_space = need_space || trailing_space;
                        continue;
                    }
                    if word == "\n" {
                        self.flush_line(&mut line, lx, &mut cursor_y, lw, max_size);
                    (cursor_y, lx, lw) = self.line_slot(cursor_y, x, width, line_guess);
                        indent = 0.0;
                        line.clear();
                        line_width = 0.0;
                        max_size = 0.0;
                        need_space = false;
                        continue;
                    }
                    let mut piece = word;
                    let mut piece_width = self.text_width(&piece, &item.style);
                    if need_space && line_width > 0.0 {
                        piece = format!(" {piece}");
                        piece_width += self.text_width(" ", &item.style);
                    }
                    if line_width + piece_width > lw - indent && line_width > 0.0 && !item.style.nowrap {
                        self.flush_line(&mut line, lx, &mut cursor_y, lw, max_size);
                    (cursor_y, lx, lw) = self.line_slot(cursor_y, x, width, line_guess);
                        indent = 0.0;
                        line.clear();
                        line_width = 0.0;
                        max_size = 0.0;
                        need_space = false;
                        piece = piece.trim_start().to_string();
                        if piece.is_empty() {
                            continue;
                        }
                        piece_width = self.text_width(&piece, &item.style);
                    }
                    max_size = max_size.max(item.style.font_size);
                    line_width += piece_width;
                    line.push((piece, item.clone_shallow()));
                    need_space = trailing_space;
                }
            }
            if line_done {
                self.flush_line(&mut line, lx, &mut cursor_y, lw, max_size);
                    (cursor_y, lx, lw) = self.line_slot(cursor_y, x, width, line_guess);
                indent = 0.0;
                line.clear();
                line_width = 0.0;
                max_size = 0.0;
                line_done = false;
                need_space = false;
            }
        }
        if !line.is_empty() {
            self.flush_line(&mut line, lx, &mut cursor_y, lw, max_size);
            cursor_y = self.line_slot(cursor_y, x, width, line_guess).0;
        }
        self.pending_indent = 0.0;
        cursor_y - y
    }

    fn flush_line(
        &mut self,
        line: &mut Vec<(String, InlineItem)>,
        x: f32,
        cursor_y: &mut f32,
        width: f32,
        max_size: f32,
    ) {
        let indent = std::mem::take(&mut self.pending_indent);
        if line.is_empty() {
            *cursor_y += max_size.max(16.0) * 1.35;
            return;
        }
        let size = if max_size > 0.0 { max_size } else { 16.0 };
        let ascent = self.fonts.ascent(size, false);
        let descent = self.fonts.descent(size, false);
        // Each run asks for its own line-height; the line is as tall as the
        // tallest. `normal` keeps the leading under the text (as before); an
        // explicit line-height splits it above and below (half-leading).
        let mut line_box = 0.0f32;
        let mut explicit = false;
        for (_, item) in line.iter() {
            let size = item.style.font_size;
            line_box = line_box.max(match item.style.line_height {
                LineHeight::Normal => (self.fonts.ascent(size, false) + self.fonts.descent(size, false)) * 1.2,
                LineHeight::Factor(k) => {
                    explicit = true;
                    k * size
                }
                LineHeight::Px(px) => {
                    explicit = true;
                    px
                }
            });
        }
        let top_gap = if explicit { (line_box - (ascent + descent)) / 2.0 } else { 0.0 };
        // Images sit on the baseline; an inline-block puts its own baseline
        // there. Tall ones push the baseline down, deep ones the line's bottom.
        let above = line.iter().filter_map(|(_, item)| item.img.as_ref().map(|i| i.atom.as_ref().map(|a| a.1).unwrap_or(i.h))).fold(0.0f32, f32::max);
        let below = line.iter().filter_map(|(_, item)| item.img.as_ref().and_then(|i| i.atom.as_ref().map(|a| i.h - a.1))).fold(0.0f32, f32::max);
        let lift = (above - ascent - top_gap.max(0.0)).max(0.0);
        let sink = (below - (line_box - top_gap.max(0.0) - ascent)).max(0.0);
        let line_height = line_box + lift + sink;

        let total_width: f32 = line.iter().map(|(text, item)| self.item_width(text, item)).sum();

        let align = if self.measuring {
            Align::Left
        } else {
            line.last().map(|(_, item)| item.style.align).unwrap_or(Align::Left)
        };
        let mut pen_x = match align {
            Align::Left => x + indent,
            Align::Center => x + (width - total_width) / 2.0,
            Align::Right => x + width - total_width,
        };

        let baseline = *cursor_y + top_gap + ascent + lift;
        // Backgrounds for the whole line first, so no run can paint over a
        // neighbour that was already drawn.
        if baseline > 0.0 {
            let mut pen = pen_x;
            for (text, item) in line.iter() {
                let advance = self.item_width(text, item);
                if let Some(background) = item.own_background {
                    if background.a > 0 && !text.trim().is_empty() {
                        self.page.draws.push(Draw::Rect {
                            rect: Rect {
                                x: pen - item.style.padding.left,
                                y: baseline - ascent - item.style.padding.top,
                                w: advance + item.style.padding.horizontal(),
                                h: ascent + descent + item.style.padding.vertical(),
                            },
                            color: background,
                        });
                    }
                }
                pen += advance;
            }
        }
        for (text, item) in line.iter() {
            if let Some(img) = &item.img {
                if item.style.transparent || (item.style.hidden && img.atom.is_none()) {
                    pen_x += img.w;
                    continue;
                }
                if let Some(atom) = &img.atom {
                    let (page, atom_baseline) = (&atom.0, atom.1);
                    let (dx, dy) = (pen_x, baseline - atom_baseline);
                    for draw in &page.draws {
                        let mut draw = draw.clone();
                        translate(&mut draw, dx, dy);
                        self.page.draws.push(draw);
                    }
                    for link in &page.links {
                        let mut link = link.clone();
                        link.rect.x += dx;
                        link.rect.y += dy;
                        self.page.links.push(link);
                    }
                    for (rect, id) in &page.boxes {
                        self.page.boxes.push((Rect { x: rect.x + dx, y: rect.y + dy, ..*rect }, *id));
                    }
                    if let Some(href) = &item.href {
                        self.page.links.push(link(Rect { x: dx, y: dy, w: img.w, h: img.h }, href));
                    }
                    for id in &item.owners {
                        self.page.boxes.push((Rect { x: dx, y: dy, w: img.w, h: img.h }, *id));
                    }
                    self.page.text.extend(page.text.iter().cloned());
                    pen_x += img.w;
                    self.page.content_right = self.page.content_right.max(pen_x);
                    continue;
                }
                let rect = Rect { x: pen_x, y: baseline - img.h, w: img.w, h: img.h };
                self.page.draws.push(Draw::Image { rect, image: img.bitmap.clone(), alt: img.alt.clone() });
                if let Some(href) = &item.href {
                    self.page.links.push(link(rect, href));
                }
                for id in &item.owners {
                    self.page.boxes.push((rect, *id));
                }
                if !img.alt.is_empty() {
                    self.page.text.push(img.alt.clone());
                }
                pen_x += img.w;
                self.page.content_right = self.page.content_right.max(pen_x);
                continue;
            }
            if text.trim().is_empty() {
                pen_x += self.text_width(text, &item.style);
                continue;
            }
            let advance = self.text_width(text, &item.style);
            for id in &item.owners {
                self.page.boxes.push((Rect { x: pen_x, y: baseline - ascent, w: advance, h: ascent + descent }, *id));
            }
            if item.style.hidden || item.style.transparent {
                pen_x += advance;
                continue;
            }
            self.page.draws.push(Draw::Text {
                x: pen_x,
                baseline,
                text: text.clone(),
                size: item.style.font_size,
                color: item.style.color,
                bold: item.style.bold,
                italic: item.style.italic,
                underline: item.style.underline,
                letter_spacing: item.style.letter_spacing,
                family: item.style.family.clone(),
            });
            if let Some(href) = &item.href {
                let piece = link(Rect { x: pen_x, y: baseline - ascent, w: advance.max(2.0), h: ascent + descent }, href);
                // the words of one link on one line are one rectangle
                match self.page.links.last_mut() {
                    Some(last) if last.href == piece.href && last.node == piece.node
                        && (last.rect.y - piece.rect.y).abs() < 0.5 && (last.rect.h - piece.rect.h).abs() < 0.5
                        && piece.rect.x >= last.rect.x && piece.rect.x <= last.rect.x + last.rect.w + piece.rect.h =>
                    {
                        last.rect.w = piece.rect.x + piece.rect.w - last.rect.x;
                    }
                    _ => self.page.links.push(piece),
                }
            }
            self.page.text.push(text.clone());
            pen_x += advance;
        }
        self.page.content_right = self.page.content_right.max(pen_x);
        *cursor_y += line_height;
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// One cell placed in the table grid.
struct Cell<'n> {
    node: &'n Node,
    ancestors: Vec<&'n Node>,
    style: Style,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
    pad: Edges,
    border: f32,
    min: f32,
    max: f32,
}

/// A table row's computed style (its background and height).
struct TableRow {
    style: Style,
}

fn span_attr(node: &Node, name: &str) -> usize {
    node.attr(name).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 1000)
}

fn px_attr(node: &Node, name: &str) -> Option<f32> {
    node.attr(name).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).filter(|v| v.is_finite() && *v >= 0.0)
}

impl Page {
    /// The element under a page point: a link's `<a>`, else the smallest
    /// element box around it. (node, raw href)
    pub fn element_at(&self, px: f32, py: f32) -> Option<(usize, Option<String>)> {
        let link = self.links.iter().find(|l| l.rect.contains(px, py));
        let boxed = self
            .boxes
            .iter()
            .filter(|(r, _)| r.contains(px, py))
            .min_by(|a, b| (a.0.w * a.0.h).partial_cmp(&(b.0.w * b.0.h)).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, id)| *id);
        let node = link.and_then(|l| l.node).or(boxed)?;
        Some((node, link.map(|l| l.href.clone())))
    }

    /// Top of the element a `#fragment` names (`top` = page start).
    pub fn anchor(&self, fragment: &str) -> Option<f32> {
        let decoded = percent_decode(fragment);
        if decoded.is_empty() || decoded.eq_ignore_ascii_case("top") {
            return Some(0.0);
        }
        self.anchors.iter().find(|(name, _)| *name == decoded).map(|(_, y)| *y)
    }

    /// Every place `query` appears in the drawn text (case-insensitive),
    /// in reading order: the boxes find-in-page highlights.
    pub fn find(&self, fonts: &Fonts, query: &str) -> Vec<Rect> {
        let needle: Vec<char> = query.to_lowercase().chars().collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for draw in &self.draws {
            let Draw::Text { x, baseline, text, size, bold, italic, letter_spacing, family, .. } = draw else { continue };
            let lower: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
            let original: Vec<char> = text.chars().collect();
            // lowercasing can change lengths (rare); then positions are approximate
            let same = lower.len() == original.len();
            let mut i = 0;
            while i + needle.len() <= lower.len() {
                if lower[i..i + needle.len()] == needle[..] {
                    let (a, b) = if same { (i, i + needle.len()) } else { (i.min(original.len()), (i + needle.len()).min(original.len())) };
                    let before: String = original[..a].iter().collect();
                    let word: String = original[a..b].iter().collect();
                    let advance = |t: &str| fonts.advance_in(t, *size, *bold, *italic, family.as_deref()) + letter_spacing * t.chars().count() as f32;
                    let x0 = x + advance(&before);
                    hits.push(Rect { x: x0 - 1.0, y: baseline - size * 0.92, w: advance(&word) + 2.0, h: size * 1.2 });
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        hits.sort_by(|a, b| (a.y, a.x).partial_cmp(&(b.y, b.x)).unwrap_or(std::cmp::Ordering::Equal));
        hits
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && s.is_char_boundary(i + 3) {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A draw that paints nothing (holds a place to fill in later).
fn blank_draw() -> Draw {
    Draw::Rect { rect: Rect { x: 0.0, y: -1e9, w: 0.0, h: 0.0 }, color: Color { r: 0, g: 0, b: 0, a: 0 } }
}

thread_local! {
    static TINTED: std::cell::RefCell<std::collections::HashMap<(usize, [u8; 4]), Arc<crate::images::Bitmap>>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// A mask image filled with `color`, made once per (image, colour).
fn tinted_mask(bitmap: &Arc<crate::images::Bitmap>, color: Color) -> Arc<crate::images::Bitmap> {
    let key = (Arc::as_ptr(bitmap) as usize, [color.r, color.g, color.b, color.a]);
    TINTED.with(|t| {
        t.borrow_mut().entry(key).or_insert_with(|| Arc::new(bitmap.tinted(key.1))).clone()
    })
}

/// background-image: sized (auto/cover/contain/lengths), positioned, and cut
/// to `area`; one copy (no tiling).
fn background_draw(bitmap: &Arc<crate::images::Bitmap>, area: Rect, style: &Style) -> Option<Draw> {
    use crate::style::{BgPos, BgSize};
    if area.w < 1.0 || area.h < 1.0 {
        return None;
    }
    let (nw, nh) = bitmap.natural_size();
    if nw < 0.5 || nh < 0.5 {
        return None;
    }
    let ratio = nw / nh;
    let (w, h) = match style.bg_size {
        BgSize::Auto => (nw, nh),
        BgSize::Cover => {
            let s = (area.w / nw).max(area.h / nh);
            (nw * s, nh * s)
        }
        BgSize::Contain => {
            let s = (area.w / nw).min(area.h / nh);
            (nw * s, nh * s)
        }
        BgSize::Sized(sw, sh) => {
            let resolve = |v: f32, full: f32| if v.is_nan() { None } else if v < 0.0 { Some(-v * full) } else { Some(v) };
            match (resolve(sw, area.w), resolve(sh, area.h)) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w / ratio),
                (None, Some(h)) => (h * ratio, h),
                (None, None) => (nw, nh),
            }
        }
    };
    if w < 0.5 || h < 0.5 {
        return None;
    }
    let place = |p: BgPos, free: f32| match p {
        BgPos::Frac(f) => free * f,
        BgPos::Px(px) => px,
    };
    let x = area.x + place(style.bg_pos.0, area.w - w);
    let y = area.y + place(style.bg_pos.1, area.h - h);
    // the part of the placed image inside the area
    let (vx0, vy0) = (x.max(area.x), y.max(area.y));
    let (vx1, vy1) = ((x + w).min(area.x + area.w), (y + h).min(area.y + area.h));
    if vx1 - vx0 < 0.5 || vy1 - vy0 < 0.5 {
        return None;
    }
    let (px_w, px_h) = (bitmap.width as f32 / w, bitmap.height as f32 / h);
    let crop = bitmap.crop(((vx0 - x) * px_w) as usize, ((vy0 - y) * px_h) as usize, ((vx1 - vx0) * px_w).round() as usize, ((vy1 - vy0) * px_h).round() as usize);
    Some(Draw::Image { rect: Rect { x: vx0, y: vy0, w: vx1 - vx0, h: vy1 - vy0 }, image: Some(crop), alt: String::new() })
}

fn translate(draw: &mut Draw, dx: f32, dy: f32) {
    match draw {
        Draw::Rect { rect, .. }
        | Draw::Border { rect, .. }
        | Draw::Image { rect, .. }
        | Draw::Fill { rect, .. }
        | Draw::Edges { rect, .. }
        | Draw::Shadow { rect, .. } => {
            rect.x += dx;
            rect.y += dy;
        }
        Draw::Text { x, baseline, .. } => {
            *x += dx;
            *baseline += dy;
        }
    }
}

impl<'a> Layout<'a> {
    fn sub_layout(&self, measuring: bool) -> Layout<'a> {
        Layout {
            fonts: self.fonts,
            sheet: self.sheet,
            viewport_width: self.viewport_width,
            images: self.images,
            measuring,
            intrinsic: measuring || self.intrinsic,
            opacity_slots: Vec::new(),
            pending_indent: 0.0,
            last_margin_bottom: 0.0,
            trailing_margin: 0.0,
            canvas_node: None,
            flow_width: self.flow_width,
            blockify: None,
            flex_main: None,
            containers: Vec::new(),
            exclusions: Vec::new(),
            page: Page::default(),
        }
    }

    /// Move what was drawn in these ranges (a positioned box) by (dx, dy).
    fn shift(&mut self, draws: std::ops::Range<usize>, links: std::ops::Range<usize>, boxes: std::ops::Range<usize>, dx: f32, dy: f32) {
        for draw in &mut self.page.draws[draws] {
            translate(draw, dx, dy);
        }
        for link in &mut self.page.links[links] {
            link.rect.x += dx;
            link.rect.y += dy;
        }
        for (rect, _) in &mut self.page.boxes[boxes] {
            rect.x += dx;
            rect.y += dy;
        }
    }

    /// overflow: hidden. Draws from `first_draw` on that fall wholly outside
    /// `rect` on a clipped axis are blanked (kept as empty draws so ranges
    /// recorded elsewhere stay valid); boxes reaching out are cut to it.
    fn clip(&mut self, first_draw: usize, first_link: usize, rect: Rect, clip_x: bool, clip_y: bool) {
        let (left, right, top, bottom) = (rect.x, rect.x + rect.w, rect.y, rect.y + rect.h);
        let fonts = self.fonts;
        let outside = |x0: f32, x1: f32, y0: f32, y1: f32| (clip_x && (x1 <= left || x0 >= right)) || (clip_y && (y1 <= top || y0 >= bottom));
        let blank = Draw::Rect { rect: Rect { x: 0.0, y: -1e9, w: 0.0, h: 0.0 }, color: Color { r: 0, g: 0, b: 0, a: 0 } };
        for draw in &mut self.page.draws[first_draw..] {
            let gone = match draw {
                Draw::Text { x, baseline, text, size, bold, italic, family, .. } => {
                    let w = fonts.advance_in(text, *size, *bold, *italic, family.as_deref());
                    outside(*x, *x + w, *baseline - *size * 0.8, *baseline + *size * 0.25)
                }
                Draw::Image { rect: r, .. } | Draw::Shadow { rect: r, .. } | Draw::Edges { rect: r, .. } => outside(r.x, r.x + r.w, r.y, r.y + r.h),
                Draw::Fill { rect: r, .. } | Draw::Rect { rect: r, .. } | Draw::Border { rect: r, .. } => {
                    if outside(r.x, r.x + r.w, r.y, r.y + r.h) {
                        true
                    } else {
                        // cut solid boxes to the clip
                        if clip_x {
                            let (a, b) = (r.x.max(left), (r.x + r.w).min(right));
                            r.x = a;
                            r.w = b - a;
                        }
                        if clip_y {
                            let (a, b) = (r.y.max(top), (r.y + r.h).min(bottom));
                            r.y = a;
                            r.h = b - a;
                        }
                        false
                    }
                }
            };
            if gone {
                *draw = blank.clone();
            }
        }
        for link in &mut self.page.links[first_link..] {
            let r = link.rect;
            if outside(r.x, r.x + r.w, r.y, r.y + r.h) {
                link.rect = Rect { x: 0.0, y: -1e9, w: 0.0, h: 0.0 };
            }
        }
    }

    /// A box laid out on its own at the origin, shrink-to-fit unless sized:
    /// (page, outer width, outer height). Tables get the table algorithm.
    fn atom_of(&self, node: &Node, with_node: &[&Node], style: &Style) -> Option<(Page, f32, f32)> {
        if style.display == Display::Table {
            let mut sub = self.sub_layout(false);
            let parent = &with_node[..with_node.len() - 1];
            let h = sub.layout_table(node, parent, style, Align::Left, 0.0, 0.0, self.flow_width.max(1.0));
            let w = (self.extent(&sub.page) + style.margin.right).min(self.flow_width.max(1.0));
            return Some((sub.page, w, h));
        }
        self.make_atom(node, with_node, style).map(|(page, _, w, h)| (page, w, h))
    }

    /// The part of a line [y, y + h) between `x` and `x + width` left free by
    /// floats: (left, width, the next y where that changes, if a float narrows it).
    fn band(&self, y: f32, h: f32, x: f32, width: f32) -> (f32, f32, Option<f32>) {
        let (mut l, mut r) = (x, x + width);
        let mut next: Option<f32> = None;
        for e in &self.exclusions {
            if e.top < y + h && e.bottom > y + 0.01 && e.x1 > x && e.x0 < x + width {
                if e.right {
                    r = r.min(e.x0);
                } else {
                    l = l.max(e.x1);
                }
                next = Some(next.map_or(e.bottom, |n: f32| n.min(e.bottom)));
            }
        }
        (l, (r - l).max(0.0), next)
    }

    /// Where the next line goes: (y, left, width), moved down below floats
    /// when they leave too little room beside them.
    fn line_slot(&self, y: f32, x: f32, width: f32, line_h: f32) -> (f32, f32, f32) {
        let mut y = y;
        for _ in 0..32 {
            let (l, w, next) = self.band(y, line_h, x, width);
            match next {
                Some(below) if w < (width * 0.25).min(80.0) => y = below,
                _ => return (y, l, w.max(1.0)),
            }
        }
        (y, x, width)
    }

    /// Put a sub-layout's page (laid out at the origin) at (x, y) on this one.
    fn append_page(&mut self, sub: Page, x: f32, y: f32) {
        self.page.draws.extend(sub.draws.into_iter().map(|mut draw| {
            translate(&mut draw, x, y);
            draw
        }));
        self.page.links.extend(sub.links.into_iter().map(|mut link| {
            link.rect.x += x;
            link.rect.y += y;
            link
        }));
        self.page.boxes.extend(sub.boxes.into_iter().map(|(mut rect, id)| {
            rect.x += x;
            rect.y += y;
            (rect, id)
        }));
        self.page.text.extend(sub.text);
        self.page.content_right = self.page.content_right.max(x + sub.content_right);
        self.page.anchors.extend(sub.anchors.into_iter().map(|(name, top)| (name, top + y)));
    }

    /// Height the pending inline items would take (for an absolute box's static position).
    fn measure_inline_height(&self, items: &[InlineItem], width: f32) -> f32 {
        let mut probe = self.sub_layout(true);
        probe.layout_inline(items, 0.0, 0.0, width)
    }

    /// `position: absolute | fixed`: out of the flow, placed by left/top/
    /// right/bottom against the nearest positioned ancestor (fixed: the
    /// first screen), shrink-to-fit unless sized or stretched by left+right.
    /// It takes no room where it sits; it is drawn over what came before.
    /// `ancestors` ends with the parent; (static_x, static_y) is where it
    /// would have gone in the flow, used when no offset is given.
    fn place_out_of_flow(&mut self, node: &Node, ancestors: &[&Node], style: &Style, static_x: f32, static_y: f32) {
        if self.intrinsic {
            return;
        }
        let fixed = style.position == crate::style::Position::Fixed;
        let (cb_x, cb_y, cb_w) = match self.containers.last() {
            Some(c) if !fixed => (c.x, c.y, c.w),
            _ => (0.0, 0.0, self.viewport_width),
        };
        let mut style = style.clone();
        if style.width.is_none() {
            if let (Some(l), Some(r)) = (style.left, style.right) {
                style.width = Some((cb_w - l - r - style.margin.horizontal()).max(1.0));
                style.border_box = true;
            }
        }
        let mut with_node: Vec<&Node> = ancestors.to_vec();
        with_node.push(node);
        let saved_flow = std::mem::replace(&mut self.flow_width, cb_w.max(1.0));
        let atom = self.atom_of(node, &with_node, &style);
        self.flow_width = saved_flow;
        let Some((sub, w, h)) = atom else { return };
        let x = match (style.left, style.right) {
            (Some(l), _) => cb_x + l,
            (None, Some(r)) => cb_x + cb_w - r - w,
            _ => static_x,
        };
        let mut bottom_fix = None;
        let y = match (style.top, style.bottom) {
            (Some(t), _) => cb_y + t,
            (None, Some(b)) if fixed || self.containers.is_empty() => crate::style::viewport_height() - b - h,
            (None, Some(b)) => {
                bottom_fix = Some(b);
                cb_y
            }
            _ => static_y,
        };
        let (d, l, bx) = (self.page.draws.len(), self.page.links.len(), self.page.boxes.len());
        self.append_page(sub, x, y);
        if let Some(b) = bottom_fix {
            let ranges = (d..self.page.draws.len(), l..self.page.links.len(), bx..self.page.boxes.len());
            if let Some(container) = self.containers.last_mut() {
                container.bottom_fixes.push((ranges.0, ranges.1, ranges.2, b, h));
            }
        }
    }

    /// Lay out a cell's contents alone at `width`: (page at the origin, height).
    fn cell_page(&self, cell: &Cell, width: f32, measuring: bool) -> (Page, f32) {
        let mut sub = self.sub_layout(measuring);
        let mut ancestors = cell.ancestors.clone();
        ancestors.push(cell.node);
        let height = sub.layout_children(cell.node, &ancestors, &cell.style, 0.0, 0.0, width.max(1.0), 0.0);
        (sub.page, height)
    }

    /// How wide the content actually is: the right edge of its text and images.
    fn extent(&self, page: &Page) -> f32 {
        page.draws.iter().fold(page.content_right, |acc, draw| match draw {
            Draw::Text { x, text, size, bold, italic, letter_spacing, family, .. } => {
                acc.max(x + self.fonts.advance_in(text, *size, *bold, *italic, family.as_deref()) + letter_spacing * text.chars().count() as f32)
            }
            Draw::Image { rect, .. } => acc.max(rect.x + rect.w),
            // sized boxes count (an empty 10px square is still 10px wide);
            // boxes stretched across the measuring width do not
            Draw::Fill { rect, .. } | Draw::Edges { rect, .. } if rect.x + rect.w < 50_000.0 => acc.max(rect.x + rect.w),
            _ => acc,
        })
    }

    /// The CSS 2 table model, automatic layout: cells on a grid (colspan,
    /// rowspan), column widths from each column's min- and max-content width
    /// fitted to the space, rows as tall as their tallest cell, cells
    /// aligned vertically inside their row. `border`, `cellspacing`,
    /// `cellpadding` and `border-collapse` are honoured.
    /// `outer_align` is the text alignment around the table (a `<center>`),
    /// which positions the table box itself.
    #[allow(clippy::too_many_arguments)]
    fn layout_table(&mut self, node: &Node, ancestors: &[&Node], style: &Style, outer_align: Align, x: f32, y: f32, containing_width: f32) -> f32 {
        let margin = style.margin;
        let avail = (containing_width - margin.horizontal()).max(1.0);
        let frame = match node.attr("border") {
            Some(v) if v.trim().is_empty() => 1.0,
            Some(_) => px_attr(node, "border").unwrap_or(1.0),
            None => style.border_width,
        };
        let spacing = if style.border_collapse { 0.0 } else { px_attr(node, "cellspacing").unwrap_or(2.0) };
        let cellpadding = px_attr(node, "cellpadding").unwrap_or(1.0);
        // the HTML border attribute rules every cell; a CSS border on the table does not
        let cell_border = if frame > 0.0 && node.attr("border").is_some() { 1.0 } else { 0.0 };

        // ----- rows and cells on the grid ---------------------------------
        let mut table_ancestors: Vec<&Node> = ancestors.to_vec();
        table_ancestors.push(node);
        let mut rows: Vec<(&Node, Vec<&Node>, Style)> = Vec::new();
        for child in node.children.iter().filter(|c| c.text.is_none()) {
            let child_style = compute(child, Some(style), self.sheet, &table_ancestors, avail);
            match child_style.display {
                Display::Row => rows.push((child, table_ancestors.clone(), child_style)),
                Display::RowGroup => {
                    let mut group_ancestors = table_ancestors.clone();
                    group_ancestors.push(child);
                    for row in child.children.iter().filter(|c| c.text.is_none()) {
                        let row_style = compute(row, Some(&child_style), self.sheet, &group_ancestors, avail);
                        if row_style.display == Display::Row {
                            rows.push((row, group_ancestors.clone(), row_style));
                        }
                    }
                }
                _ => {}
            }
        }
        if rows.is_empty() {
            // no rows: CSS wraps the content in an anonymous row and cell, so the
            // table is a shrink-to-fit box around it (figure { display: table })
            let has_content = node.children.iter().any(|c| c.text.as_ref().map_or(true, |t| !t.trim().is_empty()));
            if !has_content {
                return margin.vertical();
            }
            let mut block = style.clone();
            block.display = Display::Block;
            let mut with_node: Vec<&Node> = ancestors.to_vec();
            with_node.push(node);
            let saved = std::mem::replace(&mut self.flow_width, containing_width.max(1.0));
            // a caption (figcaption as table-caption) takes the table's width, it
            // does not set it: measure the rest, then lay everything out that wide
            let captions: Vec<bool> = node.children.iter().map(|c| c.text.is_none() && compute(c, Some(style), self.sheet, &with_node, containing_width).caption).collect();
            if captions.iter().any(|c| *c) && block.width.is_none() {
                let body = Node {
                    tag: node.tag.clone(),
                    attrs: node.attrs.clone(),
                    children: node.children.iter().zip(&captions).filter(|(_, c)| !**c).map(|(n, _)| n.clone()).collect(),
                    text: None,
                };
                let mut body_ancestors: Vec<&Node> = ancestors.to_vec();
                body_ancestors.push(&body);
                if let Some((_, _, w, _)) = self.make_atom(&body, &body_ancestors, &block) {
                    let chrome = style.padding.horizontal() + style.border.left + style.border.right;
                    block.width = Some((w - style.margin.horizontal() - if block.border_box { 0.0 } else { chrome }).max(1.0));
                }
            }
            let atom = self.make_atom(node, &with_node, &block);
            self.flow_width = saved;
            let Some((page, _, w, h)) = atom else { return margin.vertical() };
            let dx = match outer_align {
                Align::Center if !style.margin_auto.0 => (containing_width - w) / 2.0,
                _ if style.margin_auto.0 && style.margin_auto.1 => (containing_width - w) / 2.0,
                _ if style.margin_auto.0 => containing_width - w,
                _ => 0.0,
            };
            self.paste(page, x + dx.max(0.0), y);
            return h;
        }

        let mut cells: Vec<Cell> = Vec::new();
        let mut table_rows: Vec<TableRow> = Vec::new();
        // columns still taken by a rowspan from above: remaining rows per column
        let mut taken: Vec<usize> = Vec::new();
        for (r, (row, row_ancestors, row_style)) in rows.iter().enumerate() {
            let mut cell_ancestors = row_ancestors.clone();
            cell_ancestors.push(row);
            let mut col = 0usize;
            for cell in row.children.iter().filter(|c| c.text.is_none()) {
                let mut cell_style = compute(cell, Some(row_style), self.sheet, &cell_ancestors, avail);
                if cell_style.display != Display::Cell {
                    continue;
                }
                if cell_style.valign.is_none() {
                    cell_style.valign = row_style.valign;
                }
                while taken.get(col).copied().unwrap_or(0) > 0 {
                    col += 1;
                }
                let colspan = span_attr(cell, "colspan").min(64);
                let rowspan = span_attr(cell, "rowspan").min(rows.len() - r);
                if taken.len() < col + colspan {
                    taken.resize(col + colspan, 0);
                }
                for t in &mut taken[col..col + colspan] {
                    *t = rowspan;
                }
                let pad = if cell_style.padding == Edges::default() { Edges::all(cellpadding) } else { cell_style.padding };
                let border = if cell_style.border_width > 0.0 { cell_style.border_width } else { cell_border };
                cells.push(Cell {
                    node: cell,
                    ancestors: cell_ancestors.clone(),
                    style: cell_style,
                    row: r,
                    col,
                    colspan,
                    rowspan,
                    pad,
                    border,
                    min: 0.0,
                    max: 0.0,
                });
                col += colspan;
            }
            for t in taken.iter_mut() {
                *t = t.saturating_sub(1);
            }
            table_rows.push(TableRow { style: row_style.clone() });
        }
        let columns = cells.iter().map(|c| c.col + c.colspan).max().unwrap_or(0);
        if columns == 0 {
            return margin.vertical();
        }

        // ----- column widths ------------------------------------------------
        for i in 0..cells.len() {
            let chrome = cells[i].pad.horizontal() + cells[i].border * 2.0;
            let (narrow, _) = self.cell_page(&cells[i], 1.0, true);
            let (wide, _) = self.cell_page(&cells[i], 100_000.0, true);
            let mut min = self.extent(&narrow) + chrome;
            let mut max = self.extent(&wide).max(min - chrome) + chrome;
            if let Some(fixed) = cells[i].style.width {
                min = min.max(fixed);
                max = fixed.max(min);
            }
            cells[i].min = min;
            cells[i].max = max;
        }
        let mut col_min = vec![0.0f32; columns];
        let mut col_max = vec![0.0f32; columns];
        let mut col_fixed = vec![false; columns];
        for cell in cells.iter().filter(|c| c.colspan == 1) {
            col_min[cell.col] = col_min[cell.col].max(cell.min);
            col_max[cell.col] = col_max[cell.col].max(cell.max);
            if cell.style.width.is_some() {
                col_fixed[cell.col] = true;
            }
        }
        // spanning cells widen the columns they cover, evenly, if they need to
        for cell in cells.iter().filter(|c| c.colspan > 1) {
            let range = cell.col..cell.col + cell.colspan;
            let inner = spacing * (cell.colspan - 1) as f32;
            let have_min: f32 = col_min[range.clone()].iter().sum::<f32>() + inner;
            if cell.min > have_min {
                let add = (cell.min - have_min) / cell.colspan as f32;
                col_min[range.clone()].iter_mut().for_each(|w| *w += add);
            }
            let have_max: f32 = col_max[range.clone()].iter().sum::<f32>() + inner;
            if cell.max > have_max {
                let add = (cell.max - have_max) / cell.colspan as f32;
                col_max[range].iter_mut().for_each(|w| *w += add);
            }
        }
        for c in 0..columns {
            col_max[c] = col_max[c].max(col_min[c]);
        }
        let chrome = spacing * (columns + 1) as f32 + frame * 2.0;
        let target = style.width.map(|w| w.min(avail.max(w))).unwrap_or(avail);
        let room = (target - chrome).max(0.0);
        let (sum_min, sum_max): (f32, f32) = (col_min.iter().sum(), col_max.iter().sum());
        let widths: Vec<f32> = if sum_max <= room {
            if style.width.is_some() && room > sum_max {
                // a declared table width: give the slack to the flexible columns
                let flexible: Vec<usize> = (0..columns).filter(|c| !col_fixed[*c]).collect();
                let pool: Vec<usize> = if flexible.is_empty() { (0..columns).collect() } else { flexible };
                let weight: f32 = pool.iter().map(|c| col_max[*c]).sum();
                let extra = room - sum_max;
                (0..columns)
                    .map(|c| {
                        if !pool.contains(&c) {
                            col_max[c]
                        } else if weight > 0.0 {
                            col_max[c] + extra * col_max[c] / weight
                        } else {
                            col_max[c] + extra / pool.len() as f32
                        }
                    })
                    .collect()
            } else {
                col_max.clone()
            }
        } else if sum_min >= room {
            col_min.clone()
        } else {
            let k = (room - sum_min) / (sum_max - sum_min).max(0.001);
            (0..columns).map(|c| col_min[c] + (col_max[c] - col_min[c]) * k).collect()
        };
        let table_w: f32 = widths.iter().sum::<f32>() + chrome;
        let mut col_x = vec![0.0f32; columns + 1];
        col_x[0] = frame + spacing;
        for c in 0..columns {
            col_x[c + 1] = col_x[c] + widths[c] + spacing;
        }

        // ----- lay cells out at their widths; row heights -------------------
        let mut laid: Vec<(Page, f32)> = Vec::with_capacity(cells.len());
        let mut row_h = vec![0.0f32; rows.len()];
        for (r, row) in table_rows.iter().enumerate() {
            if let Some(h) = row.style.height {
                row_h[r] = h;
            }
        }
        for cell in &cells {
            let cell_w = col_x[cell.col + cell.colspan] - col_x[cell.col] - spacing;
            let content_w = (cell_w - cell.pad.horizontal() - cell.border * 2.0).max(1.0);
            let (page, height) = self.cell_page(cell, content_w, false);
            // the content's own box, for vertical alignment ...
            let content_box = height + cell.pad.vertical() + cell.border * 2.0;
            // ... and the box the row must make room for
            let box_h = content_box.max(cell.style.height.unwrap_or(0.0));
            if cell.rowspan == 1 {
                row_h[cell.row] = row_h[cell.row].max(box_h);
            }
            laid.push((page, content_box));
        }
        // a rowspan cell taller than its rows stretches the last of them
        for (cell, (_, content_box)) in cells.iter().zip(&laid) {
            let box_h = content_box.max(cell.style.height.unwrap_or(0.0));
            if cell.rowspan > 1 {
                let rows_end = cell.row + cell.rowspan;
                let have: f32 = row_h[cell.row..rows_end].iter().sum::<f32>() + spacing * (cell.rowspan - 1) as f32;
                if box_h > have {
                    row_h[rows_end - 1] += box_h - have;
                }
            }
        }
        let mut row_y = vec![0.0f32; rows.len() + 1];
        row_y[0] = frame + spacing;
        for r in 0..rows.len() {
            row_y[r + 1] = row_y[r] + row_h[r] + spacing;
        }
        let table_h = row_y[rows.len()] + frame;

        // ----- place the table ----------------------------------------------
        let free = (avail - table_w).max(0.0);
        let offset = match node.attr("align").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("center") => free / 2.0,
            Some("right") => free,
            Some(_) => 0.0,
            None if outer_align == Align::Center => free / 2.0,
            None => 0.0,
        };
        let (tx, ty) = (x + margin.left + offset, y + margin.top);
        if let Some(background) = style.background {
            self.page.draws.push(Draw::Rect { rect: Rect { x: tx, y: ty, w: table_w, h: table_h }, color: background });
        }
        for (r, row) in table_rows.iter().enumerate() {
            if let Some(background) = row.style.background {
                let rect = Rect { x: tx + col_x[0], y: ty + row_y[r], w: col_x[columns] - col_x[0] - spacing, h: row_h[r] };
                self.page.draws.push(Draw::Rect { rect, color: background });
            }
        }
        for (cell, (page, content_box_h)) in cells.iter().zip(laid) {
            let cx = tx + col_x[cell.col];
            let cy = ty + row_y[cell.row];
            let cw = col_x[cell.col + cell.colspan] - col_x[cell.col] - spacing;
            let ch = row_y[cell.row + cell.rowspan] - row_y[cell.row] - spacing;
            let rect = Rect { x: cx, y: cy, w: cw, h: ch };
            if let Some(background) = cell.style.background {
                self.page.draws.push(Draw::Rect { rect, color: background });
            }
            let slack = (ch - content_box_h).max(0.0);
            let shift = match cell.style.valign.unwrap_or(VAlign::Middle) {
                VAlign::Top => 0.0,
                VAlign::Middle => slack / 2.0,
                VAlign::Bottom => slack,
            };
            let dx = cx + cell.border + cell.pad.left;
            let dy = cy + cell.border + cell.pad.top + shift;
            for mut draw in page.draws {
                translate(&mut draw, dx, dy);
                self.page.draws.push(draw);
            }
            for mut link in page.links {
                link.rect.x += dx;
                link.rect.y += dy;
                self.page.links.push(link);
            }
            for (rect, id) in page.boxes {
                self.page.boxes.push((Rect { x: rect.x + dx, y: rect.y + dy, ..rect }, id));
            }
            self.page.text.extend(page.text);
            if cell.border > 0.0 {
                let color = if cell.style.border_width > 0.0 { cell.style.border_color } else { Color::GRAY };
                self.page.draws.push(Draw::Border { rect, width: cell.border, color });
            }
        }
        if frame > 0.0 {
            let color = if style.border_width > 0.0 { style.border_color } else { Color::GRAY };
            self.page.draws.push(Draw::Border { rect: Rect { x: tx, y: ty, w: table_w, h: table_h }, width: frame, color });
        }
        table_h + margin.vertical()
    }
}

// ---------------------------------------------------------------------------
// Flexbox (subset)
// ---------------------------------------------------------------------------

/// One flex item: an element child, or a run of text (an anonymous item).
struct FlexItem<'n> {
    node: &'n Node,
    flex: crate::style::Flex,
    /// main size the item asks for before growing/shrinking (outer, margins included)
    basis: f32,
    /// smallest main size it can shrink to (min-content, outer)
    min: f32,
    /// main size after grow/shrink
    size: f32,
    /// the item's own vertical margins, to find its box when stretching
    margin_top: f32,
    margin_bottom: f32,
    /// padding + border + margin on the right: past the content's extent
    right_chrome: f32,
    /// min-width as an outer size (0 without one)
    min_outer: f32,
    /// align-self, else the container's align-items
    align: crate::style::AlignItems,
    /// a definite width the item gives itself (outer): a column item that is
    /// not stretched keeps it instead of shrinking to its content
    fixed_outer: Option<f32>,
    /// ancestors and parent style when the item comes from a `display:
    /// contents` child (None: the container's own)
    via: Option<(Vec<&'n Node>, Style)>,
}

impl<'n> FlexItem<'n> {
    fn ctx<'s>(&'s self, ancestors: &'s [&'n Node], style: &'s Style) -> (&'s [&'n Node], &'s Style) {
        match &self.via { Some((c, p)) => (c.as_slice(), p), None => (ancestors, style) }
    }
}

/// `align-items: stretch`: grow the item's own box (its background and
/// border, which span exactly from its top to its bottom margin) by `extra`.
fn stretch_box(page: &mut Page, height: f32, extra: f32, margin_top: f32, margin_bottom: f32) {
    let bottom = height - margin_bottom;
    for draw in &mut page.draws {
        if let Draw::Rect { rect, .. } | Draw::Border { rect, .. } | Draw::Fill { rect, .. } | Draw::Edges { rect, .. } | Draw::Shadow { rect, .. } = draw {
            if (rect.y - margin_top).abs() < 0.5 && (rect.y + rect.h - bottom).abs() < 0.5 {
                rect.h += extra;
            }
        }
    }
}

impl<'a> Layout<'a> {
    /// Lay `node` out as a block at the origin of a scratch page with
    /// `width` available: (page, height). Used to measure and place items.
    fn item_page(&self, node: &Node, ancestors: &[&Node], parent: &Style, width: f32, measuring: bool) -> (Page, f32) {
        let mut sub = self.sub_layout(measuring);
        // flex and grid items are blockified: an inline-block item is a block
        sub.blockify = Some(node as *const Node);
        let height = sub.layout_block(node, ancestors, Some(parent), 0.0, 0.0, width.max(1.0));
        (sub.page, height)
    }

    /// A flex row item at its resolved outer size.
    fn item_page_sized(&self, node: &Node, ancestors: &[&Node], parent: &Style, width: f32) -> (Page, f32) {
        let mut sub = self.sub_layout(false);
        sub.blockify = Some(node as *const Node);
        sub.flex_main = Some(node as *const Node);
        let height = sub.layout_block(node, ancestors, Some(parent), 0.0, 0.0, width.max(1.0));
        (sub.page, height)
    }

    fn paste(&mut self, page: Page, dx: f32, dy: f32) {
        if page.content_right > 0.0 {
            self.page.content_right = self.page.content_right.max(page.content_right + dx);
        }
        for mut draw in page.draws {
            translate(&mut draw, dx, dy);
            self.page.draws.push(draw);
        }
        for mut link in page.links {
            link.rect.x += dx;
            link.rect.y += dy;
            self.page.links.push(link);
        }
        for (rect, id) in page.boxes {
            self.page.boxes.push((Rect { x: rect.x + dx, y: rect.y + dy, ..rect }, id));
        }
        self.page.anchors.extend(page.anchors.into_iter().map(|(name, top)| (name, top + dy)));
        self.page.text.extend(page.text);
    }

    /// The children a flex or grid container lays out: its own, with each
    /// `display: contents` child replaced by that child's children (which keep
    /// it as their ancestor and inherit from it). None when there is none.
    fn contents_children<'n>(&self, node: &'n Node, ancestors: &[&'n Node], style: &Style, width: f32) -> Option<Vec<(&'n Node, Vec<&'n Node>, Style)>> {
        let any = node.children.iter().any(|c| c.text.is_none() && compute(c, Some(style), self.sheet, ancestors, width).contents);
        if !any {
            return None;
        }
        let mut out = Vec::new();
        fn walk<'n>(layout: &Layout, node: &'n Node, ancestors: &[&'n Node], style: &Style, width: f32, out: &mut Vec<(&'n Node, Vec<&'n Node>, Style)>, depth: usize) {
            for child in &node.children {
                if child.text.is_none() && depth < 32 {
                    let s = compute(child, Some(style), layout.sheet, ancestors, width);
                    if s.contents && s.display != Display::None {
                        let mut chain = ancestors.to_vec();
                        chain.push(child);
                        walk(layout, child, &chain, &s, width, out, depth + 1);
                        continue;
                    }
                }
                out.push((child, ancestors.to_vec(), style.clone()));
            }
        }
        walk(self, node, ancestors, style, width, &mut out, 0);
        Some(out)
    }

    /// The flexbox subset: `flex-direction`, `flex-wrap`, `justify-content`,
    /// `align-items`, `gap`, `flex-grow` / `flex-shrink` / `flex-basis`.
    /// Items are measured (max- and min-content), sized along the main axis,
    /// broken into lines when wrapping, then placed and aligned; returns the
    /// height used. `ancestors` ends with `node`.
    fn layout_flex(&mut self, node: &Node, ancestors: &[&Node], style: &Style, x: f32, y: f32, width: f32) -> f32 {
        use crate::style::{AlignItems, FlexDirection, Justify};
        let f = style.flex;
        let row = matches!(f.direction, FlexDirection::Row | FlexDirection::RowReverse);
        let reverse = matches!(f.direction, FlexDirection::RowReverse | FlexDirection::ColumnReverse);

        let mut items: Vec<FlexItem> = Vec::new();
        let entries: Vec<(&Node, Option<(Vec<&Node>, Style)>)> = match self.contents_children(node, ancestors, style, width) {
            Some(list) => list.into_iter().map(|(n, chain, parent)| (n, Some((chain, parent)))).collect(),
            None => node.children.iter().map(|c| (c, None)).collect(),
        };
        for (child, via) in entries {
            let (anc, parent): (&[&Node], &Style) = match &via { Some((c, p)) => (c.as_slice(), p), None => (ancestors, style) };
            let mut fixed_outer = None;
            let (item_flex, margin_top, margin_bottom, right_chrome, min_outer) = match &child.text {
                Some(text) if text.trim().is_empty() => continue,
                Some(_) => (crate::style::Flex::default(), 0.0, 0.0, 0.0, 0.0),
                None => {
                    let s = compute(child, Some(parent), self.sheet, anc, width);
                    if s.display == Display::None {
                        continue;
                    }
                    if out_of_flow(&s) {
                        self.place_out_of_flow(child, anc, &s, x, y);
                        continue;
                    }
                    let chrome = if s.border_box { 0.0 } else { s.padding.horizontal() + s.border.left + s.border.right };
                    let min_outer = s.min_width.map(|m| m + chrome + s.margin.horizontal()).unwrap_or(0.0);
                    fixed_outer = s.width.map(|w| {
                        let w = s.max_width.map_or(w, |m| w.min(m));
                        w + chrome + s.margin.horizontal()
                    });
                    (s.flex, s.margin.top, s.margin.bottom, s.padding.right + s.border.right + s.margin.right, min_outer)
                }
            };
            let align = item_flex.align_self.unwrap_or(f.align);
            items.push(FlexItem { node: child, flex: item_flex, basis: 0.0, min: 0.0, size: 0.0, margin_top, margin_bottom, right_chrome, min_outer, align, fixed_outer, via });
        }
        if reverse {
            items.reverse();
        }
        if items.is_empty() {
            return 0.0;
        }

        if !row {
            return self.layout_flex_column(items, ancestors, style, x, y, width);
        }

        // ----- measure along the main (horizontal) axis ----------------------
        for item in &mut items {
            let (anc, parent) = item.ctx(ancestors, style);
            let (wide, _) = self.item_page(item.node, anc, parent, 100_000.0, true);
            let (narrow, _) = self.item_page(item.node, anc, parent, 1.0, true);
            let (own, basis_chrome, explicit_min) = match item.node.text {
                Some(_) => (None, 0.0, false),
                // a declared width is the content box; the item's outer size adds its margins
                None => {
                    let s = compute(item.node, Some(parent), self.sheet, anc, width);
                    let chrome = if s.border_box { 0.0 } else { s.padding.horizontal() + s.border.left + s.border.right };
                    (s.width.map(|w| w + chrome + s.margin.horizontal()),
                     chrome + s.margin.horizontal(), s.min_width.is_some())
                }
            };
            let max_content = self.extent(&wide) + item.right_chrome;
            item.min = (self.extent(&narrow) + item.right_chrome).min(max_content);
            item.basis = item.flex.basis.map(|b| b + basis_chrome).or(own).unwrap_or(max_content);
            if own.is_some() && item.flex.basis.is_none() {
                item.min = item.min.min(item.basis);
            }
            // min-width is a floor for both
            item.min = if explicit_min { item.min_outer } else { item.min.max(item.min_outer) };
            item.basis = item.basis.max(item.min_outer);
        }

        // ----- lines -----------------------------------------------------------
        let gap = f.column_gap;
        let mut lines: Vec<std::ops::Range<usize>> = Vec::new();
        let mut start = 0;
        let mut used = 0.0f32;
        for i in 0..items.len() {
            let add = items[i].basis + if i > start { gap } else { 0.0 };
            if f.wrap && i > start && used + add > width {
                lines.push(start..i);
                start = i;
                used = items[i].basis;
            } else {
                used += add;
            }
        }
        lines.push(start..items.len());

        // ----- grow / shrink, place, align --------------------------------------
        let mut cursor = y;
        for (n, line) in lines.iter().enumerate() {
            let count = line.len();
            let gaps = gap * (count.saturating_sub(1)) as f32;
            let total: f32 = items[line.clone()].iter().map(|i| i.basis).sum();
            let free = width - total - gaps;
            // measuring max-content: items stay at their basis (growing into the
            // huge measuring space would push the next item out of sight)
            if free > 0.0 && (self.measuring || self.intrinsic) {
                for item in &mut items[line.clone()] {
                    item.size = item.basis;
                }
            } else if free > 0.0 {
                let grow: f32 = items[line.clone()].iter().map(|i| i.flex.grow).sum();
                for item in &mut items[line.clone()] {
                    item.size = item.basis + if grow > 0.0 { free * item.flex.grow / grow } else { 0.0 };
                }
            } else {
                // Freeze items that reach their minimum, then redistribute the
                // remaining deficit among the items that can still shrink.
                for item in &mut items[line.clone()] {
                    item.size = item.basis.max(item.min);
                }
                loop {
                    let deficit = items[line.clone()].iter().map(|i| i.size).sum::<f32>() + gaps - width;
                    let weight: f32 = items[line.clone()].iter()
                        .filter(|i| i.size > i.min)
                        .map(|i| i.flex.shrink * i.basis).sum();
                    if deficit <= 0.001 || weight <= 0.0 {
                        break;
                    }
                    let mut froze = false;
                    for item in &mut items[line.clone()] {
                        if item.size > item.min && item.flex.shrink > 0.0 {
                            let target = item.size - deficit * item.flex.shrink * item.basis / weight;
                            froze |= target < item.min;
                            item.size = target.max(item.min);
                        }
                    }
                    if !froze {
                        break;
                    }
                }
            }
            let used: f32 = items[line.clone()].iter().map(|i| i.size).sum::<f32>() + gaps;
            let leftover = (width - used).max(0.0);
            // when measuring, everything packs to the start: centring a
            // line in a 100000px measuring box would read as a huge width
            let justify = if self.measuring { Justify::Start } else { f.justify };
            let (mut pen, spacing) = match justify {
                Justify::Start => (0.0, 0.0),
                Justify::End => (leftover, 0.0),
                Justify::Center => (leftover / 2.0, 0.0),
                Justify::SpaceBetween if count > 1 => (0.0, leftover / (count - 1) as f32),
                Justify::SpaceBetween => (0.0, 0.0),
                Justify::SpaceAround => (leftover / count as f32 / 2.0, leftover / count as f32),
                Justify::SpaceEvenly => (leftover / (count + 1) as f32, leftover / (count + 1) as f32),
            };
            let laid: Vec<(Page, f32)> = items[line.clone()]
                .iter()
                .map(|item| { let (anc, parent) = item.ctx(ancestors, style); self.item_page_sized(item.node, anc, parent, item.size) })
                .collect();
            let line_h = laid.iter().map(|(_, h)| *h).fold(0.0f32, f32::max);
            for (item, (mut page, h)) in items[line.clone()].iter().zip(laid) {
                if item.align == AlignItems::Stretch && h < line_h && item.node.text.is_none() {
                    stretch_box(&mut page, h, line_h - h, item.margin_top, item.margin_bottom);
                }
                let dy = match item.align {
                    AlignItems::Center => (line_h - h) / 2.0,
                    AlignItems::End => line_h - h,
                    AlignItems::Start | AlignItems::Stretch => 0.0,
                };
                self.paste(page, x + pen, cursor + dy);
                pen += item.size + gap + spacing;
            }
            cursor += line_h;
            if n + 1 < lines.len() {
                cursor += f.row_gap;
            }
        }
        cursor - y
    }

    /// Column direction: items stack; `align-items` places them across,
    /// `justify-content` along the container's declared height, if any.
    fn layout_flex_column(&mut self, items: Vec<FlexItem>, ancestors: &[&Node], style: &Style, x: f32, y: f32, width: f32) -> f32 {
        use crate::style::{AlignItems, Justify};
        let f = style.flex;
        let mut laid: Vec<(Page, f32, f32)> = Vec::new(); // page, height, width
        let mut aligns = Vec::new();
        for item in &items {
            let item_w = if item.align == AlignItems::Stretch {
                width
            } else if let Some(fixed) = item.fixed_outer {
                fixed.min(width).max(1.0)
            } else {
                let (anc, parent) = item.ctx(ancestors, style);
                let (wide, _) = self.item_page(item.node, anc, parent, width, true);
                (self.extent(&wide) + item.right_chrome).min(width).max(1.0)
            };
            let (anc, parent) = item.ctx(ancestors, style);
            let (page, h) = self.item_page(item.node, anc, parent, item_w, false);
            laid.push((page, h, item_w));
            aligns.push(item.align);
        }
        let gaps = f.row_gap * (laid.len().saturating_sub(1)) as f32;
        let content: f32 = laid.iter().map(|(_, h, _)| *h).sum::<f32>() + gaps;
        let leftover = style.height.map(|h| (h - content).max(0.0)).unwrap_or(0.0);
        let count = laid.len();
        let justify = if self.measuring { Justify::Start } else { f.justify };
        let (mut pen, spacing) = match justify {
            Justify::Start => (0.0, 0.0),
            Justify::End => (leftover, 0.0),
            Justify::Center => (leftover / 2.0, 0.0),
            Justify::SpaceBetween if count > 1 => (0.0, leftover / (count - 1) as f32),
            Justify::SpaceBetween => (0.0, 0.0),
            Justify::SpaceAround => (leftover / count as f32 / 2.0, leftover / count as f32),
            Justify::SpaceEvenly => (leftover / (count + 1) as f32, leftover / (count + 1) as f32),
        };
        for ((page, h, w), align) in laid.into_iter().zip(aligns) {
            let align = if self.measuring { AlignItems::Start } else { align };
            let dx = match align {
                AlignItems::Center => (width - w) / 2.0,
                AlignItems::End => width - w,
                _ => 0.0,
            };
            self.paste(page, x + dx, y + pen);
            pen += h + f.row_gap + spacing;
        }
        (pen - f.row_gap - spacing).max(0.0)
    }
}

// ---------------------------------------------------------------------------
// Inline-blocks and form controls
// ---------------------------------------------------------------------------

impl<'a> Layout<'a> {
    /// Lay out an inline-block (`ancestors` ends with `node`): shrink-to-fit
    /// width unless one is given, laid out as a block in a scratch page.
    /// Returns (page, baseline, outer width, outer height). Form controls get
    /// their contents drawn here, since they have no children to show.
    fn make_atom(&self, node: &Node, ancestors: &[&Node], style: &Style) -> Option<(Page, f32, f32, f32)> {
        let parent: Vec<&Node> = ancestors[..ancestors.len() - 1].to_vec();
        let mut style = style.clone();
        // what goes inside: the element itself, or a stand-in for a control
        let stand_in;
        let content: &Node = match node.tag.as_str() {
            "input" => {
                let kind = node.attr("type").unwrap_or("text").to_ascii_lowercase();
                match kind.as_str() {
                    "hidden" => return None,
                    "checkbox" | "radio" if style.hidden || style.transparent => {
                        // opacity:0 checkboxes behind custom toggles and menus
                        return Some((Page::default(), 0.0, 0.0, 0.0));
                    }
                    "checkbox" | "radio" => {
                        let size = 13.0;
                        let mut page = Page::default();
                        let rect = Rect { x: style.margin.left.max(3.0), y: 0.0, w: size, h: size };
                        let radius = if kind == "radio" { size / 2.0 } else { 2.0 };
                        page.draws.push(Draw::Fill { rect, radius, color: Some(Color::WHITE), gradient: None });
                        page.draws.push(Draw::Edges { rect, widths: Edges::all(1.0), colors: [Color::rgb(118, 118, 118); 4], radius });
                        if node.attr("checked").is_some() {
                            let inner = Rect { x: rect.x + 3.0, y: 3.0, w: size - 6.0, h: size - 6.0 };
                            page.draws.push(Draw::Fill { rect: inner, radius: if kind == "radio" { 3.5 } else { 1.0 }, color: Some(Color::rgb(0, 117, 255)), gradient: None });
                        }
                        let w = rect.x + size + 3.0;
                        return Some((page, size - 1.0, w, size));
                    }
                    "submit" | "button" | "reset" => {
                        let label = node.attr("value").unwrap_or(match kind.as_str() {
                            "submit" => "Submit",
                            "reset" => "Reset",
                            _ => "",
                        });
                        style.background = style.background.or(Some(Color::rgb(239, 239, 239)));
                        style.align = Align::Center;
                        stand_in = text_holder(label);
                        &stand_in
                    }
                    "image" => return None,
                    _ => {
                        let value = node.attr("value").filter(|v| !v.is_empty());
                        let shown = match (value, kind.as_str()) {
                            (Some(v), "password") => "•".repeat(v.chars().count().min(40)),
                            (Some(v), _) => v.to_string(),
                            (None, _) => {
                                style.color = Color::rgb(117, 117, 117);
                                node.attr("placeholder").unwrap_or("").to_string()
                            }
                        };
                        if style.width.is_none() {
                            let chars = node.attr("size").and_then(|v| v.parse::<f32>().ok()).unwrap_or(20.0);
                            style.width = Some(chars * style.font_size * 0.55);
                            style.border_box = false;
                        }
                        style.nowrap = true;
                        stand_in = text_holder(if shown.is_empty() { "\u{a0}" } else { &shown });
                        &stand_in
                    }
                }
            }
            "select" => {
                let options: Vec<&Node> = descendants(node, "option");
                let chosen = options.iter().find(|o| o.attr("selected").is_some()).or(options.first());
                let label = chosen.map(|o| collapse_ws(&o.text_content()).trim().to_string()).unwrap_or_default();
                style.nowrap = true;
                stand_in = text_holder(&format!("{label}  ▾"));
                &stand_in
            }
            "textarea" => {
                let rows = node.attr("rows").and_then(|v| v.parse::<f32>().ok()).unwrap_or(2.0);
                let cols = node.attr("cols").and_then(|v| v.parse::<f32>().ok()).unwrap_or(20.0);
                if style.width.is_none() {
                    style.width = Some(cols * style.font_size * 0.55);
                    style.border_box = false;
                }
                if style.height.is_none() {
                    style.height = Some(rows * style.font_size * 1.3);
                    style.border_box = false;
                }
                let text = node.text_content();
                if text.trim().is_empty() {
                    style.color = Color::rgb(117, 117, 117);
                    stand_in = text_holder(node.attr("placeholder").unwrap_or("\u{a0}"));
                } else {
                    style.pre = true;
                    stand_in = text_holder(&text);
                }
                &stand_in
            }
            _ => node,
        };
        let mut content_ancestors = parent.clone();
        content_ancestors.push(content);
        let available = self.flow_width.max(1.0);
        let outer_w = if style.width.is_some() {
            // the box sizes itself; lay it out against the line's width
            available.max(1.0)
        } else {
            // shrink-to-fit: as wide as its content, no wider than the line
            // measured without its max-width: a capped box would draw its own
            // background up to the cap and read as content that wide
            let mut probe = self.sub_layout(true);
            probe.flow_width = available;
            let mut unclamped = style.clone();
            unclamped.max_width = None;
            probe.layout_box(content, &content_ancestors, &unclamped, 0.0, 0.0, 100_000.0);
            let right = style.padding.right + style.border.right + style.margin.right;
            let chrome = if style.border_box { 0.0 } else { style.padding.horizontal() + style.border.left + style.border.right };
            let mut w = (self.extent(&probe.page) + right).min(available);
            if let Some(max) = style.max_width {
                w = w.min(max + chrome + style.margin.horizontal());
            }
            if let Some(min) = style.min_width {
                w = w.max(min + if style.border_box { 0.0 } else { style.padding.horizontal() + style.border.left + style.border.right } + style.margin.horizontal());
            }
            w.max(style.margin.horizontal() + style.padding.horizontal() + style.border.left + style.border.right + 1.0)
        };
        let mut sub = self.sub_layout(false);
        sub.flow_width = outer_w;
        let mut height = sub.layout_box(content, &content_ancestors, &style, 0.0, 0.0, outer_w);
        let width = if style.width.is_some() {
            // the real outer width of a sized box
            sub.page.draws.iter().find_map(|d| match d {
                Draw::Fill { rect, .. } | Draw::Edges { rect, .. } => Some(rect.x + rect.w + style.margin.right),
                _ => None,
            }).unwrap_or_else(|| {
                let chrome = style.padding.horizontal() + style.border.left + style.border.right;
                style.width.map(|w| if style.border_box { w } else { w + chrome }).unwrap_or(outer_w) + style.margin.horizontal()
            })
        } else {
            outer_w
        };
        height = height.max(1.0);
        // baseline: the last line of text inside, else the bottom margin edge
        let baseline = sub
            .page
            .draws
            .iter()
            .filter_map(|d| match d {
                Draw::Text { baseline, .. } => Some(*baseline),
                _ => None,
            })
            .fold(None, |acc: Option<f32>, b| Some(acc.map_or(b, |a| a.max(b))))
            .unwrap_or(height - style.margin.bottom);
        Some((sub.page, baseline, width, height))
    }

    /// CSS grid, the common subset: `grid-template-columns` with px, %, fr,
    /// auto, minmax() and repeat() (also auto-fill/auto-fit), `gap`,
    /// `grid-column: span n` and `1 / -1`, auto-placement row by row,
    /// `align-items`. Returns the height used.
    fn layout_grid(&mut self, node: &Node, ancestors: &[&Node], style: &Style, x: f32, y: f32, width: f32) -> f32 {
        use crate::style::{AlignItems, GridSpan};
        let col_gap = style.flex.column_gap;
        let row_gap = style.flex.row_gap;
        let mut tracks = parse_tracks(style.grid_columns.as_deref().unwrap_or("1fr"), width, col_gap, style.font_size);
        let mut named = std::collections::HashMap::<&str, (usize, usize, usize, usize)>::new();
        if let Some(areas) = &style.grid_areas {
            let area_columns = areas.first().map_or(1, Vec::len);
            if tracks.len() < area_columns { tracks.resize(area_columns, Track::Auto); }
            for (r, row) in areas.iter().enumerate() {
                for (c, name) in row.iter().enumerate().filter(|(_, name)| name.as_str() != ".") {
                    let area = named.entry(name).or_insert((r, c, r + 1, c + 1));
                    area.2 = area.2.max(r + 1); area.3 = area.3.max(c + 1);
                }
            }
        }
        let columns = tracks.len().max(1);

        // items and their places
        struct GridItem<'n> {
            node: &'n Node,
            /// for a child of a `display: contents` element: its ancestors and parent style
            via: Option<(Vec<&'n Node>, Style)>,
            row: usize,
            col: usize,
            span: usize,
            row_span: usize,
            area: Option<String>,
            margin: (f32, f32),
            align: AlignItems,
        }
        let mut items: Vec<GridItem> = Vec::new();
        let (mut row, mut col) = (0usize, 0usize);
        let expanded = self.contents_children(node, ancestors, style, width);
        let entries: Vec<(&Node, Option<(Vec<&Node>, Style)>)> = match expanded {
            Some(list) => list.into_iter().map(|(n, chain, parent)| (n, Some((chain, parent)))).collect(),
            None => node.children.iter().map(|c| (c, None)).collect(),
        };
        for (child, via) in entries {
            let (anc, parent): (&[&Node], &Style) = match &via { Some((c, p)) => (c.as_slice(), p), None => (ancestors, style) };
            let (span, margin, align, area) = match &child.text {
                Some(t) if t.trim().is_empty() => continue,
                Some(_) => (1, (0.0, 0.0), style.flex.align, None),
                None => {
                    let s = compute(child, Some(parent), self.sheet, anc, width);
                    if s.display == Display::None {
                        continue;
                    }
                    if out_of_flow(&s) {
                        self.place_out_of_flow(child, anc, &s, x, y);
                        continue;
                    }
                    let span = match s.grid_span {
                        GridSpan::Auto => 1,
                        GridSpan::Span(n) => n.min(columns),
                        GridSpan::Full => columns,
                    };
                    (span, (s.margin.top, s.margin.bottom), s.flex.align_self.unwrap_or(style.flex.align), s.grid_area)
                }
            };
            items.push(GridItem { node: child, via, row: 0, col: 0, span, row_span: 1, area, margin, align });
        }
        if items.is_empty() {
            return 0.0;
        }

        // Named placements reserve their cells before automatic placement,
        // regardless of the order in which elements occur in the document.
        let mut occupied = std::collections::HashSet::new();
        for item in &mut items {
            if let Some(&(r0, c0, r1, c1)) = item.area.as_deref().and_then(|name| named.get(name)) {
                item.row = r0; item.col = c0; item.span = c1 - c0; item.row_span = r1 - r0;
                for r in r0..r1 { for c in c0..c1 { occupied.insert((r, c)); } }
            }
        }
        for item in &mut items {
            if item.area.as_deref().is_some_and(|name| named.contains_key(name)) { continue; }
            loop {
                if col + item.span > columns { row += 1; col = 0; }
                if (col..col + item.span).all(|c| !occupied.contains(&(row, c))) { break; }
                col += 1;
            }
            item.row = row; item.col = col;
            for c in col..col + item.span { occupied.insert((row, c)); }
            col += item.span;
            if col >= columns { row += 1; col = 0; }
        }

        // column widths: fixed tracks first, then auto (content), then fr shares
        let gaps = col_gap * (columns - 1) as f32;
        let mut widths = vec![0.0f32; columns];
        let mut auto_need = vec![0.0f32; columns];
        let mut auto_min = vec![0.0f32; columns];
        for item in items.iter().filter(|i| i.span == 1) {
            if matches!(tracks[item.col], Track::Auto | Track::MinMax(_, MaxTrack::Auto)) {
                let (anc, parent) = match &item.via { Some((c, p)) => (c.as_slice(), p), None => (ancestors, style) };
                let (wide, _) = self.item_page(item.node, anc, parent, 100_000.0, true);
                let (narrow, _) = self.item_page(item.node, anc, parent, 1.0, true);
                auto_need[item.col] = auto_need[item.col].max(self.extent(&wide));
                auto_min[item.col] = auto_min[item.col].max(self.extent(&narrow));
            }
        }
        let mut fr_total = 0.0;
        let mut used = gaps;
        for c in 0..columns {
            match tracks[c] {
                Track::Px(px) => widths[c] = px,
                Track::Fr(fr) => fr_total += fr,
                Track::MinMax(min, MaxTrack::Fr(fr)) => {
                    widths[c] = min;
                    fr_total += fr;
                }
                // starts at its minimum; grows toward the maximum below, from what is left
                Track::MinMax(min, MaxTrack::Px(_)) => widths[c] = min,
                Track::Auto | Track::MinMax(_, MaxTrack::Auto) => widths[c] = auto_need[c],
            }
            if !matches!(tracks[c], Track::Fr(_)) {
                used += widths[c];
            }
        }
        if used > width {
            // too wide: shrink auto columns toward their min-content
            let over = used - width;
            let slack: f32 = (0..columns).filter(|c| matches!(tracks[*c], Track::Auto)).map(|c| widths[c] - auto_min[c]).sum();
            if slack > 0.0 {
                for c in (0..columns).filter(|c| matches!(tracks[*c], Track::Auto)) {
                    let cut = over.min(slack) * (widths[c] - auto_min[c]) / slack;
                    widths[c] -= cut;
                    used -= cut;
                }
            }
        }
        // grow minmax(min, <length>) tracks toward their maximum with the free space
        {
            let mut free = (width - used).max(0.0);
            let growable: Vec<usize> = (0..columns).filter(|c| matches!(tracks[*c], Track::MinMax(_, MaxTrack::Px(_)))).collect();
            let mut rounds = 0;
            while free > 0.5 && rounds < 8 {
                rounds += 1;
                let open: Vec<usize> = growable.iter().copied().filter(|c| matches!(tracks[*c], Track::MinMax(_, MaxTrack::Px(max)) if widths[*c] < max)).collect();
                if open.is_empty() {
                    break;
                }
                let share = free / open.len() as f32;
                for c in open {
                    if let Track::MinMax(_, MaxTrack::Px(max)) = tracks[c] {
                        let add = share.min(max - widths[c]);
                        widths[c] += add;
                        used += add;
                        free -= add;
                    }
                }
            }
        }
        if fr_total > 0.0 {
            let room = (width - used).max(0.0);
            for c in 0..columns {
                match tracks[c] {
                    Track::Fr(fr) => widths[c] = room * fr / fr_total,
                    Track::MinMax(min, MaxTrack::Fr(fr)) => widths[c] = (min + (room - 0.0) * fr / fr_total - 0.0).max(min),
                    _ => {}
                }
            }
        }
        let mut col_x = vec![0.0f32; columns + 1];
        for c in 0..columns {
            col_x[c + 1] = col_x[c] + widths[c] + col_gap;
        }

        // lay each item out in its cell; rows as tall as their tallest item
        let rows = items.iter().map(|i| i.row + i.row_span).max().unwrap_or(1);
        let mut laid: Vec<(Page, f32)> = Vec::with_capacity(items.len());
        let mut row_h = vec![0.0f32; rows];
        for item in &items {
            let w = col_x[item.col + item.span] - col_x[item.col] - col_gap;
            let (anc, parent) = match &item.via { Some((c, p)) => (c.as_slice(), p), None => (ancestors, style) };
            let (page, h) = self.item_page(item.node, anc, parent, w.max(1.0), false);
            if item.row_span == 1 { row_h[item.row] = row_h[item.row].max(h); }
            laid.push((page, h));
        }
        for (item, (_, h)) in items.iter().zip(&laid).filter(|(item, _)| item.row_span > 1) {
            let used = row_h[item.row..item.row + item.row_span].iter().sum::<f32>() + row_gap * (item.row_span - 1) as f32;
            let extra = (h - used).max(0.0) / item.row_span as f32;
            for row in &mut row_h[item.row..item.row + item.row_span] { *row += extra; }
        }
        let mut row_y = vec![0.0f32; rows + 1];
        for r in 0..rows {
            row_y[r + 1] = row_y[r] + row_h[r] + if r + 1 < rows { row_gap } else { 0.0 };
        }
        for (item, (mut page, h)) in items.iter().zip(laid) {
            let area_height = row_h[item.row..item.row + item.row_span].iter().sum::<f32>() + row_gap * (item.row_span - 1) as f32;
            let slack = area_height - h;
            let dy = match item.align {
                AlignItems::Center => slack / 2.0,
                AlignItems::End => slack,
                AlignItems::Stretch => {
                    if slack > 0.0 && item.node.text.is_none() {
                        stretch_box(&mut page, h, slack, item.margin.0, item.margin.1);
                    }
                    0.0
                }
                AlignItems::Start => 0.0,
            };
            self.paste(page, x + col_x[item.col], y + row_y[item.row] + dy);
        }
        row_y[rows]
    }
}

/// An anonymous element holding one run of text (form controls' contents).
fn text_holder(text: &str) -> Node {
    let mut holder = Node::element("span");
    holder.children.push(Node::text_node(text));
    holder
}

fn descendants<'n>(node: &'n Node, tag: &str) -> Vec<&'n Node> {
    let mut out = Vec::new();
    for child in &node.children {
        if child.tag == tag {
            out.push(child);
        }
        out.extend(descendants(child, tag));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MaxTrack {
    Px(f32),
    Fr(f32),
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Track {
    Px(f32),
    Fr(f32),
    Auto,
    MinMax(f32, MaxTrack),
}

/// `grid-template-columns` → tracks, with repeat() expanded against `width`.
fn parse_tracks(value: &str, width: f32, gap: f32, font_size: f32) -> Vec<Track> {
    fn split(value: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0;
        let mut current = String::new();
        let mut line_name = false;
        for c in value.chars() {
            if c == '[' { line_name = true; continue; }
            if c == ']' { line_name = false; continue; }
            if line_name { continue; }
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
    let one = |t: &str| -> Track {
        let t = t.trim();
        if let Some(fr) = t.strip_suffix("fr").and_then(|v| v.parse::<f32>().ok()) {
            Track::Fr(fr)
        } else if let Some(inner) = t.strip_prefix("minmax(").and_then(|v| v.strip_suffix(')')) {
            let (lo, hi) = inner.split_once(',').unwrap_or((inner, "auto"));
            let min = crate::style::parse_length(lo.trim(), font_size, width).unwrap_or(0.0);
            let hi = hi.trim();
            let max = if let Some(fr) = hi.strip_suffix("fr").and_then(|v| v.parse::<f32>().ok()) {
                MaxTrack::Fr(fr)
            } else if let Some(px) = crate::style::parse_length(hi, font_size, width) {
                MaxTrack::Px(px)
            } else {
                MaxTrack::Auto
            };
            Track::MinMax(min, max)
        } else if let Some(px) = crate::style::parse_length(t, font_size, width) {
            Track::Px(px)
        } else {
            // auto, min-content, max-content, fit-content(): sized by content
            Track::Auto
        }
    };
    let mut tracks = Vec::new();
    for token in split(value) {
        if let Some(inner) = token.strip_prefix("repeat(").and_then(|v| v.strip_suffix(')')) {
            let Some((count, list)) = inner.split_once(',') else { continue };
            let list: Vec<Track> = split(list).iter().map(|t| one(t)).collect();
            if list.is_empty() {
                continue;
            }
            let n = match count.trim() {
                "auto-fill" | "auto-fit" => {
                    // as many as fit at their minimum size
                    let min: f32 = list.iter().map(|t| match t {
                        Track::Px(p) => *p,
                        Track::MinMax(m, _) => *m,
                        _ => 100.0,
                    }).sum::<f32>().max(1.0);
                    (((width + gap) / (min + gap)).floor() as usize).max(1)
                }
                n => n.parse::<usize>().unwrap_or(1).clamp(1, 64),
            };
            for _ in 0..n {
                tracks.extend(list.iter().copied());
            }
        } else {
            tracks.push(one(&token));
        }
    }
    if tracks.is_empty() {
        tracks.push(Track::Fr(1.0));
    }
    tracks
}

impl InlineItem {
    fn clone_shallow(&self) -> InlineItem {
        InlineItem {
            text: self.text.clone(),
            style: self.style.clone(),
            href: self.href.clone(),
            br: self.br,
            image: self.image,
            own_background: self.own_background,
            img: self.img.clone(),
            owners: self.owners.clone(),
        }
    }
}

/// The box of an `<img>`: `width`/`height` attributes, then CSS, then the
/// decoded size; one given side scales the other by the aspect ratio.
/// None when nothing gives a size (no attributes and no decodable image).
fn image_size(node: &Node, style: &Style, bitmap: Option<&Bitmap>) -> Option<(f32, f32)> {
    let attr = |name: &str| {
        node.attr(name)
            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
    };
    let w = attr("width").or(style.width.filter(|v| *v > 0.0));
    let h = attr("height").or(style.height.filter(|v| *v > 0.0));
    let natural = bitmap.map(|b| b.natural_size());
    let size = match (w, h, natural) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((nw, nh))) => (w, w * nh / nw),
        (None, Some(h), Some((nw, nh))) => (h * nw / nh, h),
        (None, None, Some(n)) => n,
        (Some(w), None, None) => (w, w),
        (None, Some(h), None) => (h, h),
        (None, None, None) => return None,
    };
    Some((size.0.min(8192.0), size.1.min(8192.0)))
}

fn default_for(node: &Node) -> Style {
    crate::style::default_style(&node.tag)
}

pub fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out
}

/// Scripts written without spaces, where any character boundary is a break opportunity.
fn breaks_anywhere(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

/// Punctuation that must not begin a line (it closes what came before).
fn is_closing_punct(ch: char) -> bool {
    matches!(ch, '。' | '，' | '、' | '．' | '：' | '；' | '！' | '？' | '）' | '」' | '』' | '】' | '〉' | '》' | 'ー' | '々')
}

/// Split into words, each carrying whether the source had whitespace *after* it.
fn split_words(text: &str, pre: bool) -> Vec<(String, bool)> {
    if pre {
        let mut out = Vec::new();
        let mut run = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                if !run.is_empty() { out.push((std::mem::take(&mut run), false)); }
                out.push(("\n".to_string(), false));
            } else {
                run.push(ch);
                if ch.is_whitespace() { out.push((std::mem::take(&mut run), false)); }
            }
        }
        if !run.is_empty() { out.push((run, false)); }
        return out;
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut leading_space = false;
    for ch in text.chars() {
        if ch == '\n' {
            if !current.is_empty() {
                out.push((current.clone(), false));
                current.clear();
            }
            out.push(("\n".into(), false));
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                out.push((current.clone(), true));
                current.clear();
            } else if let Some(last) = out.last_mut() {
                last.1 = true;
            } else {
                leading_space = true;
            }
        } else if breaks_anywhere(ch) {
            // CJK: a line may break before and after each character
            // (closing punctuation stays with the character before it)
            if is_closing_punct(ch) {
                match out.last_mut() {
                    Some(last) if current.is_empty() && !last.1 && last.0 != "\n" => last.0.push(ch),
                    _ => {
                        current.push(ch);
                        out.push((std::mem::take(&mut current), false));
                    }
                }
                continue;
            }
            if !current.is_empty() {
                out.push((std::mem::take(&mut current), false));
            }
            current.push(ch);
            out.push((std::mem::take(&mut current), false));
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        out.push((current, false));
    }
    if leading_space {
        out.insert(0, (String::new(), true));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_document;

    fn fonts() -> Fonts {
        Fonts::load()
    }

    #[test]
    fn collapses_whitespace() {
        assert_eq!(collapse_ws("a   b\n\tc"), "a b c");
        assert_eq!(collapse_ws("  "), " ");
    }

    #[test]
    fn cjk_text_breaks_between_characters() {
        let words: Vec<String> = split_words("该域名仅用于。ok", false).into_iter().map(|w| w.0).collect();
        assert_eq!(words, vec!["该", "域", "名", "仅", "用", "于。", "ok"]);
    }

    #[test]
    fn grid_minmax_length_tracks_leave_room_for_content_columns() {
        let fonts = fonts();
        let doc = parse_document("<body style='margin:0'><div style='display:grid;grid-template-columns:minmax(0,900px) min-content;width:600px'><div>main text</div><div><div style='width:150px'>side</div></div></div></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let side = page.draws.iter().find_map(|d| match d { Draw::Text { x, text, .. } if text.contains("side") => Some(*x), _ => None }).unwrap();
        assert!(side > 400.0 && side < 460.0, "side column at {side}");
    }

    #[test]
    fn growing_flex_items_do_not_inflate_a_measured_width() {
        let fonts = fonts();
        let doc = parse_document("<body style='margin:0'><div style='display:flex;flex-wrap:wrap;width:800px'><div style='width:200px'>start</div><div style='display:flex;flex-grow:1'><div style='flex-grow:1'>field</div><div>Search</div></div></div></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let y = |w: &str| page.draws.iter().find_map(|d| match d { Draw::Text { baseline, text, .. } if text.contains(w) => Some(*baseline), _ => None }).unwrap();
        assert_eq!(y("start"), y("Search"), "the growing item wrapped to a second line");
    }

    #[test]
    fn inline_buttons_draw_their_children() {
        let fonts = fonts();
        let doc = parse_document("<body><p>menu <button style='display:inline'><span>HTML</span></button></p></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let texts: Vec<String> = page.draws.iter().filter_map(|d| match d { Draw::Text { text, .. } => Some(text.clone()), _ => None }).collect();
        assert!(texts.iter().any(|t| t.contains("HTML")) && !texts.iter().any(|t| t.contains('[')), "{texts:?}");
    }

    #[test]
    fn display_contents_children_are_flex_items() {
        let fonts = fonts();
        let doc = parse_document("<body style='margin:0'><div style='display:flex;width:800px'><div>left</div><div style='display:contents'><div>mid</div><div>right</div></div></div></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let pos = |w: &str| page.draws.iter().find_map(|d| match d { Draw::Text { x, baseline, text, .. } if text.contains(w) => Some((*x, *baseline)), _ => None }).unwrap();
        let (l, m, r) = (pos("left"), pos("mid"), pos("right"));
        assert_eq!(l.1, r.1, "contents children wrapped to new lines");
        assert!(l.0 < m.0 && m.0 < r.0, "{l:?} {m:?} {r:?}");
    }

    #[test]
    fn row_less_tables_shrink_to_their_content() {
        let fonts = fonts();
        let doc = parse_document("<body style='margin:0'><figure style='display:table;margin:0;float:right'><img width=100 height=50><figcaption>cap</figcaption></figure><p>text</p></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let cap = page.draws.iter().find_map(|d| match d { Draw::Text { x, text, .. } if text.contains("cap") => Some(*x), _ => None }).unwrap();
        assert!(cap > 600.0, "caption of a floated row-less table at {cap}");
    }

    #[test]
    fn table_captions_wrap_to_the_table_width() {
        let fonts = fonts();
        let doc = parse_document("<body style='margin:0'><figure style='display:table;margin:0;float:right'><span style='display:inline-block;width:120px;height:40px'></span><figcaption style='display:table-caption'>a long caption that is much wider than the picture above it</figcaption></figure><p>text</p></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let xs: Vec<f32> = page.draws.iter().filter_map(|d| match d { Draw::Text { x, text, .. } if !text.contains("text") => Some(*x), _ => None }).collect();
        assert!(xs.iter().all(|x| *x > 650.0), "caption lines start at {xs:?}");
    }

    #[test]
    fn svg_icons_with_a_display_stay_pictures() {
        let fonts = fonts();
        let doc = parse_document("<head><style>.i{display:inline-block;fill:currentColor} b{display:flex}</style></head><body><b><svg class=i viewBox='0 0 16 16' width=16 height=16><circle cx=8 cy=8 r=7></circle></svg>x</b><p><svg class=i viewBox='0 0 16 16' width=20 height=20><circle cx=8 cy=8 r=7></circle></svg></p></body>");
        let sheet = crate::css::Stylesheet::parse(".i{display:inline-block;fill:currentColor} b{display:flex}");
        let page = Layout::new(&fonts, &sheet, 400.0).build(&doc);
        let pictures = page.draws.iter().filter(|d| matches!(d, Draw::Image { rect, .. } if rect.w >= 15.0)).count();
        assert_eq!(pictures, 2, "both icons drawn as pictures");
    }

    #[test]
    fn a_link_on_one_line_is_one_rectangle() {
        let fonts = fonts();
        let doc = parse_document("<body><p><a href=/a>one two three</a> and <a href=/b>four five</a></p></body>");
        let sheet = crate::css::Stylesheet::default();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        assert_eq!(page.links.len(), 2, "{:?}", page.links.iter().map(|l| (&l.href, l.rect)).collect::<Vec<_>>());
    }

    #[test]
    fn splits_words() {
        assert_eq!(
            split_words("one two", false),
            vec![("one".to_string(), true), ("two".to_string(), false)]
        );
        assert_eq!(
            split_words(" x", false),
            vec![(String::new(), true), ("x".to_string(), false)]
        );
        assert_eq!(
            split_words("a\nb", false),
            vec![("a".to_string(), false), ("\n".to_string(), false), ("b".to_string(), false)]
        );
    }

    #[test]
    fn absolute_boxes_leave_the_flow_and_sit_on_their_container() {
        let doc = parse_document(
            "<body style='margin:0'>\
             <div style='position:relative;height:100px;background:#eee'>\
               <span style='position:absolute;right:10px;top:5px;width:20px;height:20px;background:red'></span>\
               <div style='position:absolute;left:0;right:0;bottom:0;height:10px;background:blue'></div>\
             </div>\
             <p style='margin:0'>after</p>\
             <div style='position:fixed;bottom:0;left:0;right:0;height:50px;background:green'></div></body>",
        );
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let fill = |c: Color| page.draws.iter().find_map(|d| match d {
            Draw::Fill { rect, color: Some(col), .. } if *col == c => Some(*rect),
            _ => None,
        }).unwrap_or_else(|| panic!("no fill {c:?}"));
        let red = fill(Color::rgb(255, 0, 0));
        assert_eq!((red.x, red.y, red.w), (770.0, 5.0, 20.0), "right/top against the relative box");
        let blue = fill(Color::rgb(0, 0, 255));
        assert_eq!((blue.x, blue.y, blue.w), (0.0, 90.0, 800.0), "left+right stretch, bottom anchors after height is known");
        let green = fill(Color::rgb(0, 128, 0));
        assert_eq!((green.y, green.w), (800.0 - 50.0, 800.0), "fixed sits at the bottom of the first screen");
        let after = page.draws.iter().find_map(|d| match d {
            Draw::Text { text, baseline, .. } if text.contains("after") => Some(*baseline),
            _ => None,
        }).unwrap();
        assert!(after > 100.0 && after < 125.0, "absolutes take no room: {after}");
    }

    #[test]
    fn floats_sit_side_by_side_and_the_parent_holds_them() {
        let doc = parse_document(
            "<body style='margin:0'><div id=w>\
               <div style='float:left;width:30%;height:40px;background:red'></div>\
               <div style='float:left;width:30%;height:60px;background:blue'></div>\
               <div style='float:right;width:100px;height:20px;background:green'></div>\
             </div><p style='margin:0'>below</p></body>",
        );
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 1000.0).build(&doc);
        let fill = |c: Color| page.draws.iter().find_map(|d| match d {
            Draw::Fill { rect, color: Some(col), .. } if *col == c => Some(*rect),
            _ => None,
        }).unwrap();
        assert_eq!((fill(Color::rgb(255, 0, 0)).x, fill(Color::rgb(255, 0, 0)).y), (0.0, 0.0));
        assert_eq!((fill(Color::rgb(0, 0, 255)).x, fill(Color::rgb(0, 0, 255)).y), (300.0, 0.0));
        assert_eq!(fill(Color::rgb(0, 128, 0)).x, 900.0);
        let below = page.draws.iter().find_map(|d| match d {
            Draw::Text { text, baseline, .. } if text.contains("below") => Some(*baseline),
            _ => None,
        }).unwrap();
        assert!(below > 60.0 && below < 85.0, "text starts under the tallest float: {below}");
    }

    #[test]
    fn overflow_hidden_keeps_its_height_and_hides_what_spills() {
        let doc = parse_document(
            "<body style='margin:0'><div style='overflow:hidden;height:30px'>\
               <div style='height:20px;background:red'></div>\
               <div style='height:20px;background:blue'></div>\
               <div style='height:20px;background:green'></div>\
             </div><p style='margin:0'>after</p></body>",
        );
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 400.0).build(&doc);
        let fill = |c: Color| page.draws.iter().find_map(|d| match d {
            Draw::Fill { rect, color: Some(col), .. } if *col == c => Some(*rect),
            _ => None,
        });
        assert_eq!(fill(Color::rgb(0, 0, 255)).map(|r| (r.y, r.h)), Some((20.0, 10.0)), "cut at the edge");
        assert!(fill(Color::rgb(0, 128, 0)).is_none(), "wholly outside: not drawn");
        let after = page.draws.iter().find_map(|d| match d {
            Draw::Text { text, baseline, .. } if text.contains("after") => Some(*baseline),
            _ => None,
        }).unwrap();
        assert!(after > 30.0 && after < 55.0, "the box stays 30px: {after}");
    }

    #[test]
    fn text_flows_beside_a_float_then_under_it() {
        let words = "word ".repeat(200);
        let html = format!("<body style='margin:0'><div style='float:right;width:300px;height:100px;background:red'></div><p style='margin:0'>{words}</p></body>");
        let doc = parse_document(&html);
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 1000.0).build(&doc);
        let red = page.draws.iter().find_map(|d| match d {
            Draw::Fill { rect, color: Some(c), .. } if *c == Color::rgb(255, 0, 0) => Some(*rect),
            _ => None,
        }).unwrap();
        assert_eq!((red.x, red.y), (700.0, 0.0));
        let texts: Vec<(f32, f32, f32)> = page.draws.iter().filter_map(|d| match d {
            Draw::Text { x, baseline, text, size, bold, italic, family, .. } => Some((*baseline, *x, *x + fonts.advance_in(text, *size, *bold, *italic, family.as_deref()))),
            _ => None,
        }).collect();
        assert!(texts.iter().any(|t| t.0 < 20.0), "first line starts at the top, beside the float");
        assert!(texts.iter().filter(|t| t.0 < 95.0).all(|t| t.2 <= 700.5), "no text under the float");
        assert!(texts.iter().any(|t| t.0 > 110.0 && t.2 > 750.0), "below it lines are full width again");
    }

    #[test]
    fn anchors_and_find_in_page() {
        let doc = parse_document("<body style='margin:0'><p style='margin:0;height:500px'>Intro rust text</p><h2 id='History' style='margin:0'>History of Rust</h2><a name='x'></a></body>");
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        assert_eq!(page.anchor("History"), Some(500.0));
        assert_eq!(page.anchor("top"), Some(0.0));
        assert_eq!(page.anchor("missing"), None);
        let hits = page.find(&fonts, "RUST");
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits[0].y < 30.0 && hits[1].y > 490.0, "reading order: {hits:?}");
        assert!(hits[0].x > 20.0 && hits[0].w > 10.0, "inside the run, not at its start: {hits:?}");
    }

    #[test]
    fn background_images_cover_contain_and_position() {
        let bmp = crate::images::decode(include_bytes!("../../docs/fixtures/img/gradient.png")).expect("png");
        let (nw, nh) = bmp.natural_size();
        let mut map = ImageMap::new();
        map.insert("https://x.test/bg.png".to_string(), Arc::new(bmp));
        let html = "<body style='margin:0'>\
            <div style='width:400px;height:100px;background:#000 url(\"https://x.test/bg.png\") center / cover no-repeat'></div>\
            <div style='width:400px;height:100px;background-image:url(https://x.test/bg.png);background-size:contain;background-position:right bottom'></div></body>";
        let doc = parse_document(html);
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).with_images(&map).build(&doc);
        let images: Vec<Rect> = page.draws.iter().filter_map(|d| match d {
            Draw::Image { rect, image: Some(_), .. } => Some(*rect),
            _ => None,
        }).collect();
        assert_eq!(images.len(), 2, "{images:?}");
        let cover = images[0];
        assert_eq!((cover.x, cover.y, cover.w, cover.h), (0.0, 0.0, 400.0, 100.0), "cover fills the box, cut to it");
        let contain = images[1];
        let s = (400.0 / nw).min(100.0 / nh);
        assert!((contain.w - nw * s).abs() < 0.5 && (contain.h - nh * s).abs() < 0.5, "{contain:?}");
        assert!((contain.x + contain.w - 400.0).abs() < 0.5 && (contain.y + contain.h - 200.0).abs() < 0.5, "right bottom: {contain:?}");
    }

    #[test]
    fn a_block_table_still_lays_out_rows_as_rows() {
        let doc = parse_document("<table style='display:block'><tr><th>Label</th><td>Value</td></tr></table>");
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 600.0).build(&doc);
        let at = |w: &str| page.draws.iter().find_map(|d| match d {
            Draw::Text { x, baseline, text, .. } if text.contains(w) => Some((*x, *baseline)),
            _ => None,
        }).unwrap();
        let (label, value) = (at("Label"), at("Value"));
        assert_eq!(label.1, value.1, "same row");
        assert!(value.0 > label.0 + 20.0);
    }

    #[test]
    fn keeps_spaces_across_inline_boundaries() {
        let doc = parse_document("<p>alpha <b>beta</b> gamma</p>");
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let line = page.text.join("");
        assert!(line.contains("alpha"), "line: {line:?}");
        assert!(
            page.text.iter().any(|t| t.trim() == "gamma" && t.starts_with(' ')),
            "space before gamma lost: {:?}",
            page.text
        );
    }

    #[test]
    fn renders_paragraphs_and_headings() {
        let doc = parse_document("<h1>Title</h1><p>Hello <b>bold</b> world</p>");
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        assert!(page.height > 20.0);
        let texts: Vec<String> = page
            .draws
            .iter()
            .filter_map(|d| match d {
                Draw::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("Title")));
        assert!(texts.iter().any(|t| t.contains("Hello")));
    }

    #[test]
    fn wraps_long_lines() {
        let long = "word ".repeat(200);
        let doc = parse_document(&format!("<p>{long}</p>"));
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let narrow = Layout::new(&fonts, &sheet, 200.0).build(&doc);
        let wide = Layout::new(&fonts, &sheet, 2000.0).build(&doc);
        assert!(narrow.height > wide.height * 2.0, "narrow page must be taller");
    }

    #[test]
    fn links_are_collected() {
        let doc = parse_document(r#"<p><a href="/x">click</a></p>"#);
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        assert_eq!(page.links.len(), 1);
        assert_eq!(page.links[0].href, "/x");
    }

    fn image_map(src: &str, w: u32, h: u32) -> ImageMap {
        let png = crate::images::tests::png_bytes(w, h, [10, 20, 30, 255]);
        let mut map = ImageMap::new();
        map.insert(src.to_string(), Arc::new(crate::images::decode(&png).unwrap()));
        map
    }

    fn image_rects(page: &Page) -> Vec<(Rect, bool)> {
        page.draws
            .iter()
            .filter_map(|d| match d {
                Draw::Image { rect, image, .. } => Some((*rect, image.is_some())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn images_take_their_natural_or_declared_size() {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let map = image_map("a.png", 120, 60);
        let doc = parse_document(r#"<p><img src="a.png"> <img src="a.png" width="60"> <img src="a.png" style="height:15px"></p>"#);
        let page = Layout::new(&fonts, &sheet, 800.0).with_images(&map).build(&doc);
        let rects = image_rects(&page);
        assert_eq!(rects.len(), 3);
        assert_eq!((rects[0].0.w, rects[0].0.h), (120.0, 60.0));
        assert_eq!((rects[1].0.w, rects[1].0.h), (60.0, 30.0), "width keeps aspect");
        assert_eq!((rects[2].0.w, rects[2].0.h), (30.0, 15.0), "css height keeps aspect");
        assert!(rects.iter().all(|(_, loaded)| *loaded));
        // all on one line, bottoms on the same baseline
        let bottom = rects[0].0.y + rects[0].0.h;
        assert!(rects.iter().all(|(r, _)| (r.y + r.h - bottom).abs() < 0.01));
        assert!(page.height >= 60.0, "the line grew to fit the image");
    }

    #[test]
    fn wide_images_shrink_to_the_container() {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let map = image_map("big.png", 2000, 1000);
        let doc = parse_document(r#"<div><img src="big.png"></div>"#);
        let page = Layout::new(&fonts, &sheet, 516.0).with_images(&map).build(&doc);
        let (rect, _) = image_rects(&page)[0];
        assert!((rect.w - 500.0).abs() < 0.01, "width {}", rect.w);
        assert!((rect.h - 250.0).abs() < 0.01, "height {}", rect.h);
    }

    #[test]
    fn linked_image_is_clickable_and_missing_one_keeps_alt() {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let map = image_map("logo.png", 40, 20);
        let doc = parse_document(
            r#"<p><a href="/home"><img src="logo.png"></a> <img src="gone.png" width="80" height="30" alt="Chart"> <img src="gone.png" alt="Plain alt"> <img src="gone.png"></p>"#,
        );
        let page = Layout::new(&fonts, &sheet, 800.0).with_images(&map).build(&doc);
        let rects = image_rects(&page);
        assert_eq!(rects.len(), 2, "sized boxes only: {rects:?}");
        assert!(rects[0].1 && !rects[1].1, "second is a placeholder");
        assert_eq!(page.links.len(), 1);
        assert_eq!(page.links[0].rect, rects[0].0);
        assert_eq!(page.links[0].href, "/home");
        assert!(page.text.join("").contains("Plain alt"), "unsized missing image falls back to its alt text");
        assert!(page.text.iter().any(|t| t == "Chart"));
    }

    fn text_x(page: &Page, needle: &str) -> (f32, f32) {
        page.draws
            .iter()
            .find_map(|d| match d {
                Draw::Text { x, baseline, text, .. } if text.trim() == needle => Some((*x, *baseline)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{needle:?} not drawn"))
    }

    #[test]
    fn table_cells_form_columns() {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let doc = parse_document(
            "<table><tr><td>a</td><td>first</td></tr><tr><td>much longer cell</td><td>b</td></tr></table>",
        );
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let (ax, ay) = text_x(&page, "a");
        let (lx, ly) = text_x(&page, "much");
        let (fx, _) = text_x(&page, "first");
        let (bx, by) = text_x(&page, "b");
        assert_eq!(ax, lx, "column 1 is left-aligned in both rows");
        assert_eq!(fx, bx, "column 2 starts at the same x in both rows");
        assert!(fx > lx + fonts.advance("much longer cell", 16.0, false, false), "column 1 is as wide as its widest cell");
        assert!(ly > ay && by == ly, "rows stack, cells of a row share a line");
    }

    #[test]
    fn colspan_valign_and_bgcolor() {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let doc = parse_document(
            r##"<table cellspacing="0" cellpadding="0" bgcolor="#ff6600" width="400">
                <tr><td colspan="2">header across both</td></tr>
                <tr><td height="100">left</td><td valign="top">right</td></tr>
                <tr><td>x</td><td align="right">end</td></tr>
            </table>"##,
        );
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let orange = page.draws.iter().find_map(|d| match d {
            Draw::Rect { rect, color } if color.r == 255 && color.g == 102 => Some(*rect),
            _ => None,
        });
        let table = orange.expect("bgcolor paints the table");
        assert!((table.w - 400.0).abs() < 0.5, "width=400 honoured: {}", table.w);
        let (lx, ly) = text_x(&page, "left");
        let (rx, ry) = text_x(&page, "right");
        let (hx, _) = text_x(&page, "header");
        assert_eq!(hx, lx, "colspan cell starts at column 1");
        assert!(ry + 30.0 < ly, "valign=top sits above the middle-aligned neighbour ({ry} vs {ly})");
        assert!(rx > lx);
        let (ex, _) = text_x(&page, "end");
        let end_w = fonts.advance("end", 16.0, false, false);
        assert!((ex + end_w - (table.x + table.w)).abs() < 1.0, "align=right reaches the right edge");
    }

    #[test]
    fn wide_tables_fit_the_container() {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let long = "word ".repeat(80);
        let doc = parse_document(&format!("<table border=1><tr><td>{long}</td><td>{long}</td></tr></table>"));
        let page = Layout::new(&fonts, &sheet, 600.0).build(&doc);
        let right = page.draws.iter().fold(0.0f32, |acc, d| match d {
            Draw::Border { rect, .. } => acc.max(rect.x + rect.w),
            _ => acc,
        });
        assert!(right <= 600.0 - 8.0 + 0.5, "table overflowed: {right}");
        assert!(page.height > 100.0, "text wrapped inside the cells");
    }

    fn flex_page(html: &str, width: f32) -> Page {
        let fonts = fonts();
        let sheet = Stylesheet::default();
        let doc = parse_document(html);
        Layout::new(&fonts, &sheet, width).build(&doc)
    }

    #[test]
    fn flex_control_frame_uses_the_allocated_width() {
        let page = flex_page("<body style='margin:0'><div style='display:flex;width:400px;gap:10px'><input style='flex:1;min-width:0'><button style='flex:0 0 80px;box-sizing:border-box'>Go</button></div></body>", 400.0);
        let frame = page.draws.iter().find_map(|d| match d {
            Draw::Edges { rect, .. } if rect.x < 1.0 => Some(rect),
            _ => None,
        }).unwrap();
        assert!((frame.w - 310.0).abs() < 0.5, "input frame: {frame:?}");
    }

    #[test]
    fn textarea_wraps_preserved_lines_inside_its_frame() {
        let page = flex_page("<textarea style='width:120px;height:100px'>alpha beta gamma delta epsilon</textarea>", 400.0);
        let (_, first) = text_x(&page, "alpha");
        let (_, last) = text_x(&page, "epsilon");
        assert!(last > first, "textarea should soft-wrap");
        let fonts = fonts();
        assert!(page.draws.iter().filter_map(|d| match d {
            Draw::Text { x, text, size, bold, italic, .. } => Some(x + fonts.advance(text, *size, *bold, *italic)),
            _ => None,
        }).all(|right| right <= 132.5));
    }

    #[test]
    fn positioned_text_is_preserved_for_search() {
        let page = flex_page("<div style='position:absolute;left:10px;top:10px'>positioned</div>", 400.0);
        assert!(page.draws.iter().any(|d| matches!(d, Draw::Text { text, .. } if text == "positioned")));
        assert!(page.text.iter().any(|text| text == "positioned"));
    }

    #[test]
    fn relayout_rechecks_selectors_after_sibling_changes() {
        let fonts = fonts();
        let sheet = Stylesheet::parse("span { color: blue } span:first-child { color: red }");
        let mut doc = parse_document("<html><body><div><span>Alpha</span><span>Beta</span></div></body></html>");
        let color_of_beta = |page: &Page| page.draws.iter().find_map(|draw| match draw {
            Draw::Text { text, color, .. } if text == "Beta" => Some(*color),
            _ => None,
        }).unwrap();
        let before = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        assert_eq!(color_of_beta(&before), Color::rgb(0, 0, 255));
        // Keep the second node and all ancestor addresses in place. Only a
        // sibling changes, which affects :first-child on the unchanged node.
        let body = &mut doc.children[0].children[0];
        body.children[0].children[0] = Node::text_node(" ");
        let after = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        assert_eq!(color_of_beta(&after), Color::rgb(255, 0, 0));
    }

    #[test]
    fn grid_named_areas_override_document_order_and_reserve_auto_cells() {
        let page = flex_page(
            "<body style='margin:0'><div style='display:grid;grid-template-columns:100px 200px;gap:10px;grid-template-areas:\"header header\" \"sidebar content\"'>\
             <div style='grid-area:content'>Main</div><div style='grid-area:sidebar'>Sidebar</div>\
             <div style='grid-area:header'>Header</div><div>Automatic</div></div></body>", 400.0);
        let (mx, my) = text_x(&page, "Main");
        let (sx, sy) = text_x(&page, "Sidebar");
        let (hx, hy) = text_x(&page, "Header");
        let (ax, ay) = text_x(&page, "Automatic");
        assert!((mx - 110.0).abs() < 0.5 && sx.abs() < 0.5 && hx.abs() < 0.5);
        assert!((my - sy).abs() < 0.5 && hy < my && ay > my && ax.abs() < 0.5);
    }

    #[test]
    fn grid_shorthand_and_named_line_labels_do_not_add_columns() {
        let page = flex_page(
            "<body style='margin:0'><div style='display:grid;grid-template:\"side content\" auto / [start] 90px [middle] 210px [end];column-gap:12px'>\
             <div style='grid-area:content'>Content</div><div style='grid-area:side'>Side</div></div></body>", 400.0);
        let (x, y) = text_x(&page, "Content");
        let (sx, sy) = text_x(&page, "Side");
        assert!((x - 102.0).abs() < 0.5 && sx.abs() < 0.5 && (y - sy).abs() < 0.5);
    }

    #[test]
    fn grid_named_area_rowspan_contributes_to_all_rows() {
        let page = flex_page(
            "<body style='margin:0'><div style='display:grid;grid-template-columns:100px 200px;gap:10px;grid-template-areas:\"sidebar top\" \"sidebar bottom\"'>\
             <div style='grid-area:top;height:20px'>Top</div><div style='grid-area:bottom;height:20px'>Bottom</div>\
             <div style='grid-area:sidebar;height:100px'>Sidebar</div><div>After</div></div></body>", 400.0);
        assert!(text_x(&page, "Bottom").1 > text_x(&page, "Top").1 + 20.0);
        assert!(text_x(&page, "After").1 >= text_x(&page, "Top").1 + 100.0);
    }

    #[test]
    fn opacity_resets_without_resetting_visibility_or_ancestor_opacity() {
        let page = flex_page(
            "<div style='opacity:0;opacity:1'>reset</div>\
             <div style='visibility:hidden;opacity:1'>hidden</div>\
             <div style='opacity:0'><span style='visibility:visible;opacity:1'>transparent</span></div>\
             <div style='visibility:hidden'><span style='visibility:visible'>visible</span></div>", 400.0);
        let text = page.text.join(" ");
        assert!(text.contains("reset") && text.contains("visible"), "{text}");
        assert!(!text.contains("hidden") && !text.contains("transparent"), "{text}");
    }

    #[test]
    fn flex_basis_includes_box_chrome() {
        for (sizing, expected) in [("content-box", 140.0), ("border-box", 120.0)] {
            let page = flex_page(&format!(
                "<body style='margin:0'><div style='display:flex;width:400px'>\
                 <div style='box-sizing:{sizing};flex:0 0 100px;padding:0 10px;margin:0 10px'>A</div>\
                 <div>B</div></div></body>"), 400.0);
            let (x, _) = text_x(&page, "B");
            assert!((x - expected).abs() < 0.5, "{sizing}: {x}");
        }
    }

    #[test]
    fn flex_explicit_zero_minimum_allows_shrinking() {
        let page = flex_page(
            "<body style='margin:0'><div style='display:flex;width:100px'>\
             <div style='flex:0 1 200px;min-width:0'>averyveryveryverylongword</div>\
             <div style='flex:0 0 20px'>B</div></div></body>", 300.0);
        assert!((text_x(&page, "B").0 - 80.0).abs() < 0.5);
    }

    #[test]
    fn flex_shrink_redistributes_after_minimum_is_reached() {
        let page = flex_page(
            "<body style='margin:0'><div style='display:flex;width:200px'>\
             <div style='width:200px;min-width:150px'>A</div>\
             <div style='width:200px;min-width:0'>B</div>\
             <div style='flex:0 0 0px;min-width:0'>C</div></div></body>", 400.0);
        assert!((text_x(&page, "B").0 - 150.0).abs() < 0.5);
        assert!((text_x(&page, "C").0 - 200.0).abs() < 0.5);
    }

    #[test]
    fn flex_row_places_items_side_by_side() {
        let page = flex_page("<div style='display:flex;gap:10px'><div>one</div><div>two</div><div>three</div></div>", 600.0);
        let (x1, y1) = text_x(&page, "one");
        let (x2, y2) = text_x(&page, "two");
        let (x3, y3) = text_x(&page, "three");
        assert!(y1 == y2 && y2 == y3, "one line");
        let fonts = fonts();
        let w1 = fonts.advance("one", 16.0, false, false);
        assert!((x2 - (x1 + w1 + 10.0)).abs() < 0.5, "gap between items: {x1} {x2}");
        assert!(x3 > x2);
    }

    #[test]
    fn flex_grow_and_justify() {
        // grow: the middle item takes the free space, so "end" sits at the right edge
        let page = flex_page(
            "<div style='display:flex'><div>start</div><div style='flex:1'>mid</div><div>end</div></div>",
            508.0,
        );
        let fonts = fonts();
        let (ex, _) = text_x(&page, "end");
        let end_w = fonts.advance("end", 16.0, false, false);
        assert!((ex + end_w - 500.0).abs() < 0.5, "end at right edge: {}", ex + end_w);

        let page = flex_page("<div style='display:flex;justify-content:space-between'><span>L</span><span>R</span></div>", 508.0);
        let (rx, _) = text_x(&page, "R");
        assert!((rx + fonts.advance("R", 16.0, false, false) - 500.0).abs() < 0.5);

        let page = flex_page("<div style='display:flex;justify-content:center'><span>mid</span></div>", 508.0);
        let (mx, _) = text_x(&page, "mid");
        let w = fonts.advance("mid", 16.0, false, false);
        assert!((mx - (8.0 + (492.0 - w) / 2.0)).abs() < 0.5, "mid at {mx}, want {}", 8.0 + (492.0 - w) / 2.0);
    }

    #[test]
    fn flex_wrap_and_column() {
        let items: String = (0..6).map(|i| format!("<div style='width:150px'>item{i}</div>")).collect();
        let page = flex_page(&format!("<div style='display:flex;flex-wrap:wrap;row-gap:4px'>{items}</div>"), 508.0);
        let (_, y0) = text_x(&page, "item0");
        let (_, y2) = text_x(&page, "item2");
        let (_, y3) = text_x(&page, "item3");
        assert_eq!(y0, y2, "three 150px items fit in 500px");
        assert!(y3 > y0, "the fourth wraps");

        let page = flex_page("<div style='display:flex;flex-direction:column;align-items:center'><div>top</div><div>bottom</div></div>", 508.0);
        let (tx, ty) = text_x(&page, "top");
        let (_, by) = text_x(&page, "bottom");
        assert!(by > ty, "column stacks");
        assert!(tx > 150.0, "align-items:center centres across: {tx}");
    }

    #[test]
    fn flex_shrinks_to_fit_but_not_below_min_content() {
        let long = "word ".repeat(60);
        let page = flex_page(&format!("<div style='display:flex'><div>{long}</div><div>{long}</div></div>"), 408.0);
        let fonts = fonts();
        let right = page.draws.iter().fold(0.0f32, |acc, d| match d {
            Draw::Text { x, text, .. } => acc.max(x + fonts.advance(text, 16.0, false, false)),
            _ => acc,
        });
        assert!(right <= 400.0 + 8.0 + 0.5, "overflowed to {right}");
        assert!(page.height > 60.0, "text wrapped inside the shrunk items");
    }

    #[test]
    fn vertical_margins_collapse() {
        let fonts = fonts();
        let line = (fonts.ascent(16.0, false) + fonts.descent(16.0, false)) * 1.2;
        // two paragraphs: 16px between them, not 16 + 16
        let page = flex_page("<p>one</p><p>two</p>", 600.0);
        let (_, y1) = text_x(&page, "one");
        let (_, y2) = text_x(&page, "two");
        assert!((y2 - y1 - (line + 16.0)).abs() < 0.5, "gap {}", y2 - y1 - line);
        // the larger margin wins
        let page = flex_page("<p>one</p><div style='margin-top:40px'>two</div>", 600.0);
        let (_, y1) = text_x(&page, "one");
        let (_, y2) = text_x(&page, "two");
        assert!((y2 - y1 - (line + 40.0)).abs() < 0.5, "gap {}", y2 - y1 - line);
        // a parent with no border/padding collapses with its first child
        let a = flex_page("<div style='margin-top:30px'><p>x</p></div>", 600.0);
        let b = flex_page("<div style='margin-top:30px;padding-top:1px'><p>x</p></div>", 600.0);
        let (_, ya) = text_x(&a, "x");
        let (_, yb) = text_x(&b, "x");
        assert!((yb - ya - 17.0).abs() < 0.5, "collapsed {ya}, separated {yb}");
    }

    #[test]
    fn line_height_letter_spacing_and_indent() {
        let fonts = fonts();
        let page = flex_page("<p style='line-height:2'>a<br>b</p>", 600.0);
        let (_, ya) = text_x(&page, "a");
        let (_, yb) = text_x(&page, "b");
        assert!((yb - ya - 32.0).abs() < 0.5, "line-height:2 at 16px: {}", yb - ya);
        let page = flex_page("<p style='line-height:24px'>a<br>b</p>", 600.0);
        let (_, ya) = text_x(&page, "a");
        let (_, yb) = text_x(&page, "b");
        assert!((yb - ya - 24.0).abs() < 0.5);

        let plain = flex_page("<p>abcd <b>x</b></p>", 600.0);
        let spaced = flex_page("<p style='letter-spacing:3px'>abcd <b>x</b></p>", 600.0);
        let (px, _) = text_x(&plain, "x");
        let (sx, _) = text_x(&spaced, "x");
        // "abcd" before the run that starts with " x": 4 characters x 3px
        assert!((sx - px - 12.0).abs() < 0.5, "4 characters x 3px: {}", sx - px);

        let words = "word ".repeat(40);
        let page = flex_page(&format!("<p style='text-indent:30px'>{words}</p>"), 400.0);
        let firsts: Vec<f32> = page
            .draws
            .iter()
            .filter_map(|d| match d {
                Draw::Text { x, text, .. } if !text.starts_with(' ') => Some(*x),
                _ => None,
            })
            .collect();
        assert!((firsts[0] - 38.0).abs() < 0.5, "first line indented: {}", firsts[0]);
        assert!(firsts[1..].iter().any(|x| (*x - 8.0).abs() < 0.5), "later lines are not");
        let _ = fonts;
    }

    #[test]
    fn max_width_with_auto_margins_centres() {
        let page = flex_page(
            "<div style='max-width:300px;margin:0 auto;background:#eeeeee'>column</div>",
            816.0,
        );
        let rect = page
            .draws
            .iter()
            .find_map(|d| match d {
                Draw::Fill { rect, .. } => Some(*rect),
                _ => None,
            })
            .expect("background");
        assert!((rect.w - 300.0).abs() < 0.5, "width {}", rect.w);
        assert!((rect.x - (8.0 + (800.0 - 300.0) / 2.0)).abs() < 0.5, "x {}", rect.x);
    }

    #[test]
    fn display_none_is_skipped() {
        let doc = parse_document("<div style='display:none'>hidden</div><p>shown</p>");
        let sheet = Stylesheet::default();
        let fonts = fonts();
        let page = Layout::new(&fonts, &sheet, 800.0).build(&doc);
        let text = page.text.join(" ");
        assert!(!text.contains("hidden"));
        assert!(text.contains("shown"));
    }
}
