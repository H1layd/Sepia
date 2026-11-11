//! The window: Sepia's own chrome, drawn with our own rasteriser.
//!
//! Layout (logical px; multiplied by the display scale factor when drawn):
//!
//! * **capsule** (top centre) — a circle with the site's monogram and a
//!   transport dot. Hover or ⌘L opens it into the address field.
//! * **viewport** — the page, a rounded card that runs to the bottom edge;
//!   the dock floats over it and the page scrolls clear of it.
//! * **dock** (bottom centre) — new tab, the tab carousel, the transport chip
//!   (`tor` / `i2p` / `chain` / `direct`), burn.
//!   Tabs are page thumbnails standing in perspective; the one under the
//!   pointer turns to face you, lifts and grows, its neighbours make room, its
//!   monogram lights up and a plate shows the title and domain.
//!
//! There are no back / forward / reload buttons: a two-finger swipe right goes
//! back, left goes forward, pulling the page down past the top reloads.
//! ⌘[ ⌘] ⌘R do the same from the keyboard.
//!
//! `Ui` is plain state plus pure drawing into a `Canvas`, so the whole window
//! renders headless in tests. winit only delivers events, softbuffer only takes
//! the finished pixels. Pages load on a worker thread through `page::load`
//! (the one network entry point); nothing here opens a socket or touches disk.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowBuilder};

use crate::css::Color;
use crate::js::{Outcome, PageEvent};
use crate::layout::{Fonts, Page, Rect};
use crate::net::{Chain, HopKind};
use crate::page::{self, Loaded};
use crate::paint::Canvas;
use crate::resolver::Resolver;
use crate::url::Url;

// ---------------------------------------------------------------------------
// Theme: every colour and size of the chrome lives here.
// ---------------------------------------------------------------------------

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

mod theme {
    use super::rgb;
    use crate::css::Color;

    pub const WINDOW: Color = rgb(13, 15, 18);
    pub const PANEL: Color = rgb(21, 24, 29);
    pub const RAISED: Color = rgb(28, 32, 39);
    pub const RAISED_HOVER: Color = rgb(38, 43, 52);
    pub const STROKE: Color = rgb(38, 43, 51);
    pub const TEXT: Color = rgb(232, 234, 237);
    pub const TEXT_DIM: Color = rgb(154, 163, 174);
    pub const ACCENT: Color = rgb(124, 240, 192);
    pub const ON_ACCENT: Color = rgb(13, 15, 18);
    pub const DANGER: Color = rgb(255, 138, 128);
    pub const PAGE: Color = rgb(255, 255, 255);

    pub const TOR: Color = rgb(183, 148, 255);
    pub const I2P: Color = rgb(242, 193, 78);
    pub const CHAIN: Color = rgb(143, 180, 255);
    pub const DIRECT: Color = rgb(255, 150, 110);

    // geometry, logical px
    pub const TOP: f32 = 56.0;
    pub const INSET: f32 = 8.0;
    pub const VIEW_RADIUS: f32 = 14.0;
    pub const CAPSULE: f32 = 36.0;
    pub const CAPSULE_OPEN_MAX: f32 = 560.0;
    pub const DOCK_HEIGHT: f32 = 92.0;
    pub const DOCK_MARGIN: f32 = 12.0;
    pub const BUTTON: f32 = 44.0;
    pub const SLOT: f32 = 46.0;
    pub const CARD_W: f32 = 68.0;
    pub const CARD_H: f32 = 44.0;
    /// Room at each end of the tab strip: a card pushed aside by its hovered
    /// neighbour must still fit without being cut.
    pub const STRIP_PAD: f32 = 26.0;
    pub const ARROW: f32 = 24.0;
    pub const CHIP: f32 = 92.0;
    pub const MAX_VISIBLE: usize = 9;

    // motion
    pub const TILT_DEG: f32 = 52.0;
    pub const MAGNIFY: f32 = 1.5;
    pub const LIFT: f32 = 30.0;
    pub const PERSPECTIVE: f32 = 700.0;
    pub const EASE: f32 = 0.09; // seconds, time constant of every animation
}

mod text {
    pub const NEW_TAB: &str = "Новая вкладка";
    pub const START_TITLE: &str = "Sepia";
    pub const START_HINT: &str = "Наведите на кружок сверху или нажмите ⌘L, чтобы ввести адрес";
    pub const GESTURES: &str =
        "Свайп двумя пальцами вправо — назад, влево — вперёд. Потяните страницу вниз — обновить.";
    pub const ERROR_TITLE: &str = "Страница не открылась";
    pub const ERROR_RETRY: &str = "Потяните страницу вниз или нажмите ⌘R, чтобы повторить";
    pub const LOADING: &str = "Загрузка";
    pub const BAD_ADDRESS: &str = "Это не похоже на адрес";
    pub const NO_BACK: &str = "Назад некуда";
    pub const NO_FORWARD: &str = "Вперёд некуда";
    pub const BURNED: &str = "Вкладки и история стёрты из памяти";
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Transport {
    Tor,
    I2p,
    Chain(usize),
    Direct,
}

impl Transport {
    pub fn of(chain: &Chain, url: Option<&Url>) -> Transport {
        match url.and_then(|u| u.needs_overlay()) {
            Some("tor") => return Transport::Tor,
            Some(_) => return Transport::I2p,
            None => {}
        }
        let local = |host: &str| matches!(host, "127.0.0.1" | "localhost" | "::1");
        match chain.hops.as_slice() {
            [] => Transport::Direct,
            [hop] if hop.kind == HopKind::Socks5 && local(&hop.host) && matches!(hop.port, 9050 | 9150) => {
                Transport::Tor
            }
            [hop] if hop.kind == HopKind::Http && local(&hop.host) && hop.port == 4444 => Transport::I2p,
            hops => Transport::Chain(hops.len()),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Transport::Tor => "tor",
            Transport::I2p => "i2p",
            Transport::Chain(_) => "chain",
            Transport::Direct => "direct",
        }
    }

    fn color(&self) -> Color {
        match self {
            Transport::Tor => theme::TOR,
            Transport::I2p => theme::I2P,
            Transport::Chain(_) => theme::CHAIN,
            Transport::Direct => theme::DIRECT,
        }
    }

    fn describe(&self) -> String {
        match self {
            Transport::Tor => "через Tor".into(),
            Transport::I2p => "через I2P".into(),
            Transport::Chain(1) => "через прокси".into(),
            Transport::Chain(n) => format!("цепочка из {n} прокси"),
            Transport::Direct => "напрямую: сайт видит ваш адрес".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Images (tab thumbnails)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Image {
    w: usize,
    h: usize,
    rgb: Vec<u8>,
}

const THUMB_W: usize = 136;
const THUMB_H: usize = 88;

impl Image {
    /// Box-filter a canvas down to `dw`×`dh`.
    fn from_canvas(canvas: &Canvas, dw: usize, dh: usize) -> Image {
        let mut rgb = vec![0u8; dw * dh * 3];
        for y in 0..dh {
            let sy0 = y * canvas.height / dh;
            let sy1 = ((y + 1) * canvas.height / dh).max(sy0 + 1).min(canvas.height);
            for x in 0..dw {
                let sx0 = x * canvas.width / dw;
                let sx1 = ((x + 1) * canvas.width / dw).max(sx0 + 1).min(canvas.width);
                let mut sum = [0u32; 3];
                let mut n = 0u32;
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let i = (sy * canvas.width + sx) * 4;
                        for c in 0..3 {
                            sum[c] += canvas.pixels[i + c] as u32;
                        }
                        n += 1;
                    }
                }
                for c in 0..3 {
                    rgb[(y * dw + x) * 3 + c] = (sum[c] / n.max(1)) as u8;
                }
            }
        }
        Image { w: dw, h: dh, rgb }
    }

    /// Bilinear sample at normalised (u, v) in 0..1.
    fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        let x = (u * self.w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let y = (v * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (tx, ty) = (x.fract(), y.fract());
        let p = |xx: usize, yy: usize, c: usize| self.rgb[(yy * self.w + xx) * 3 + c] as f32;
        let mut out = [0.0; 3];
        for c in 0..3 {
            let top = p(x0, y0, c) * (1.0 - tx) + p(x1, y0, c) * tx;
            let bottom = p(x0, y1, c) * (1.0 - tx) + p(x1, y1, c) * tx;
            out[c] = top * (1.0 - ty) + bottom * ty;
        }
        out
    }
}

/// A page thumbnail: the top of the page at its layout width, shrunk.
pub fn thumbnail(fonts: &Fonts, page: &Page, width: f32) -> Image {
    let w = width.max(320.0);
    let h = w * THUMB_H as f32 / THUMB_W as f32;
    let mut canvas = Canvas::new(w as usize, h as usize, theme::PAGE);
    canvas.paint(fonts, page, 0.0);
    Image::from_canvas(&canvas, THUMB_W, THUMB_H)
}

// ---------------------------------------------------------------------------
// Tabs and UI state
// ---------------------------------------------------------------------------

struct Pending {
    nav: u64,
    url: Url,
    push: bool,
}

struct Failure {
    url: Url,
    reason: String,
}

/// Animated pose of one tab card; every field eases toward its target.
#[derive(Clone, Copy, Default)]
struct CardAnim {
    push: f32,
    lift: f32,
    tilt: f32,
    scale: f32,
    depth: f32,
    hot: f32,
}

struct Tab {
    id: u64,
    history: Vec<Url>,
    index: usize,
    loaded: Option<Loaded>,
    /// Bumped whenever `loaded` changes, so a cached page bitmap knows it is stale.
    generation: u64,
    /// The navigation that produced `loaded`: its script session answers to it.
    live_nav: u64,
    thumb: Option<Image>,
    loading: Option<Pending>,
    failure: Option<Failure>,
    scroll: f32,
    anim: CardAnim,
}

impl Tab {
    fn new(id: u64) -> Tab {
        Tab {
            id,
            history: Vec::new(),
            index: 0,
            loaded: None,
            generation: 0,
            live_nav: 0,
            thumb: None,
            loading: None,
            failure: None,
            scroll: 0.0,
            anim: CardAnim { tilt: theme::TILT_DEG, scale: 1.0, ..CardAnim::default() },
        }
    }

    fn url(&self) -> Option<&Url> {
        if let Some(p) = &self.loading {
            return Some(&p.url);
        }
        if let Some(f) = &self.failure {
            return Some(&f.url);
        }
        self.history.get(self.index)
    }

    fn host(&self) -> String {
        self.url().map(|u| u.host.trim_start_matches("www.").to_string()).unwrap_or_default()
    }

    fn title(&self) -> String {
        if let Some(loaded) = &self.loaded {
            if self.loading.is_none() && self.failure.is_none() && !loaded.title.trim().is_empty() {
                return loaded.title.trim().to_string();
            }
        }
        match self.url() {
            Some(url) => url.host.clone(),
            None => text::NEW_TAB.to_string(),
        }
    }

    fn monogram(&self) -> char {
        let host = self.host();
        if host.is_empty() {
            return 'S';
        }
        host.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase()).unwrap_or('•')
    }

    fn tint(&self) -> Color {
        let host = self.host();
        if host.is_empty() {
            return rgb(35, 42, 51);
        }
        let mut hash: u32 = 2166136261;
        for b in host.bytes() {
            hash = (hash ^ b as u32).wrapping_mul(16777619);
        }
        hsl((hash % 360) as f32, 0.30, 0.26)
    }
}

fn hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    rgb(to(r), to(g), to(b))
}

/// Something the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    Nothing,
    Capsule,
    NewTab,
    Prev,
    Next,
    Tab(usize),
    Transport,
    Burn,
    Dock,
    Page,
}

/// A page load the window should start on a worker thread.
#[derive(Debug, Clone)]
pub struct Load {
    pub tab_id: u64,
    pub nav: u64,
    pub url: Url,
}

#[derive(Clone, Copy)]
struct Geom {
    viewport: Rect,
    capsule: Rect,
    dock: Rect,
    new_tab: Rect,
    prev: Rect,
    strip: Rect,
    next: Rect,
    chip: Rect,
    burn: Rect,
    visible: usize,
    arrows: bool,
}

pub struct Ui {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    next_nav: u64,
    transport: Transport,
    doh: bool,
    w: f32,
    h: f32,
    hover: Hit,
    pub editing: bool,
    input: String,
    replace_input: bool,
    addr_open: f32,
    offset: usize,
    offset_anim: f32,
    wheel_acc: f32,
    /// Horizontal swipe progress: +1 = go back, −1 = go forward.
    swipe: f32,
    /// Pull-to-reload progress, 1 = reload on release.
    pull: f32,
    gesture_active: bool,
    spin: f32,
    toast: Option<(String, f32)>,
    link: Option<String>,
}

fn ease(v: &mut f32, target: f32, dt: f32) -> bool {
    let k = 1.0 - (-dt / theme::EASE).exp();
    let next = *v + (target - *v) * k;
    let moving = (target - next).abs() > 0.002;
    *v = if moving { next } else { target };
    moving
}

impl Ui {
    pub fn new(transport: Transport, doh: bool) -> Ui {
        let mut ui = Ui {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            next_nav: 1,
            transport,
            doh,
            w: 1200.0,
            h: 900.0,
            hover: Hit::Nothing,
            editing: false,
            input: String::new(),
            replace_input: false,
            addr_open: 0.0,
            offset: 0,
            offset_anim: 0.0,
            wheel_acc: 0.0,
            swipe: 0.0,
            pull: 0.0,
            gesture_active: false,
            spin: 0.0,
            toast: None,
            link: None,
        };
        ui.push_tab();
        ui
    }

    pub fn set_size(&mut self, w: f32, h: f32) {
        self.w = w.max(320.0);
        self.h = h.max(240.0);
        self.clamp_offset();
    }

    fn push_tab(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab::new(id));
        self.tabs.len() - 1
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some((message.to_string(), 2.2));
    }

    // ----- geometry -------------------------------------------------------

    fn geom(&self) -> Geom {
        let (w, h) = (self.w, self.h);
        // the page runs to the top; the capsule floats over it
        let viewport = Rect {
            x: theme::INSET,
            y: theme::INSET,
            w: w - theme::INSET * 2.0,
            h: h - theme::INSET * 2.0,
        };
        let open_w = theme::CAPSULE_OPEN_MAX.min(w - 32.0);
        let t = smooth(self.addr_open);
        let cap_w = theme::CAPSULE + (open_w - theme::CAPSULE) * t;
        let capsule = Rect { x: (w - cap_w) / 2.0, y: theme::INSET + 10.0, w: cap_w, h: theme::CAPSULE };

        let fixed = |arrows: bool| {
            12.0 + theme::BUTTON + 8.0 + 1.0 + 8.0
                + if arrows { theme::ARROW * 2.0 } else { 0.0 }
                + theme::STRIP_PAD * 2.0
                + 8.0 + 1.0 + 8.0 + theme::CHIP + 8.0 + theme::BUTTON + 12.0
        };
        let n = self.tabs.len().max(1);
        let mut visible = n.min(theme::MAX_VISIBLE);
        let mut arrows = n > visible;
        while visible > 3 && fixed(arrows) + visible as f32 * theme::SLOT > w - 24.0 {
            visible -= 1;
            arrows = n > visible;
        }
        let dock_w = fixed(arrows) + visible as f32 * theme::SLOT;
        let dock = Rect {
            x: (w - dock_w) / 2.0,
            y: h - theme::DOCK_MARGIN - theme::DOCK_HEIGHT,
            w: dock_w,
            h: theme::DOCK_HEIGHT,
        };
        let row = |x: f32, width: f32| Rect { x, y: dock.y + (dock.h - theme::BUTTON) / 2.0, w: width, h: theme::BUTTON };
        let mut x = dock.x + 12.0;
        let new_tab = row(x, theme::BUTTON);
        x += theme::BUTTON + 8.0 + 1.0 + 8.0;
        let arrow_w = if arrows { theme::ARROW } else { 0.0 };
        let prev = row(x, arrow_w);
        x += arrow_w;
        let strip = Rect {
            x,
            y: dock.y,
            w: theme::STRIP_PAD * 2.0 + visible as f32 * theme::SLOT,
            h: dock.h,
        };
        x += strip.w;
        let next = row(x, arrow_w);
        x += arrow_w + 8.0 + 1.0 + 8.0;
        let chip = row(x, theme::CHIP);
        x += theme::CHIP + 8.0;
        let burn = row(x, theme::BUTTON);
        Geom { viewport, capsule, dock, new_tab, prev, strip, next, chip, burn, visible, arrows }
    }

    fn slot_center(&self, g: &Geom, i: usize) -> f32 {
        g.strip.x + theme::STRIP_PAD + (i as f32 - self.offset_anim + 0.5) * theme::SLOT
    }

    pub fn viewport(&self) -> Rect {
        self.geom().viewport
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let g = self.geom();
        let inside = |r: &Rect, pad: f32| {
            x >= r.x - pad && x <= r.x + r.w + pad && y >= r.y - pad && y <= r.y + r.h + pad
        };
        if inside(&g.capsule, if self.addr_open > 0.5 { 8.0 } else { 6.0 }) {
            return Hit::Capsule;
        }
        // The hovered card pokes above the dock; keep it hovered while the
        // pointer is over its enlarged face.
        if let Hit::Tab(i) = self.hover {
            let cx = self.slot_center(&g, i);
            let half = theme::CARD_W * theme::MAGNIFY / 2.0;
            let top = g.dock.y + 12.0 - theme::LIFT - theme::CARD_H * theme::MAGNIFY;
            let in_strip = x >= g.strip.x && x <= g.strip.x + g.strip.w;
            if x >= cx - half && x <= cx + half && y >= top && y <= g.dock.y + g.dock.h && in_strip {
                return Hit::Tab(i);
            }
        }
        if inside(&g.dock, 0.0) {
            if inside(&g.new_tab, 0.0) {
                return Hit::NewTab;
            }
            if g.arrows && inside(&g.prev, 0.0) {
                return Hit::Prev;
            }
            if g.arrows && inside(&g.next, 0.0) {
                return Hit::Next;
            }
            if inside(&g.chip, 0.0) {
                return Hit::Transport;
            }
            if inside(&g.burn, 0.0) {
                return Hit::Burn;
            }
            if inside(&g.strip, 0.0) {
                let rel = (x - g.strip.x - theme::STRIP_PAD) / theme::SLOT + self.offset as f32;
                if rel >= 0.0 {
                    let i = rel as usize;
                    if i < self.tabs.len() && i >= self.offset && i < self.offset + g.visible {
                        return Hit::Tab(i);
                    }
                }
            }
            return Hit::Dock;
        }
        if inside(&g.viewport, 0.0) {
            return Hit::Page;
        }
        Hit::Nothing
    }

    pub fn set_hover(&mut self, hit: Hit) {
        self.hover = hit;
    }

    // ----- navigation -----------------------------------------------------

    fn start_load(&mut self, index: usize, url: Url, push: bool) -> Load {
        let nav = self.next_nav;
        self.next_nav += 1;
        let tab = &mut self.tabs[index];
        tab.loading = Some(Pending { nav, url: url.clone(), push });
        Load { tab_id: tab.id, nav, url }
    }

    /// Parse what was typed in the address field and load it in the active tab.
    pub fn submit(&mut self) -> Option<Load> {
        let typed = self.input.trim().to_string();
        let looks_like_address = !typed.is_empty()
            && !typed.contains(char::is_whitespace)
            && (typed.contains("://") || typed.contains('.') || typed.starts_with("localhost"));
        match Url::parse(&typed).filter(|_| looks_like_address) {
            Some(url) => {
                self.editing = false;
                Some(self.start_load(self.active, url, true))
            }
            None => {
                self.toast(text::BAD_ADDRESS);
                None
            }
        }
    }

    pub fn navigate(&mut self, url: Url) -> Load {
        self.start_load(self.active, url, true)
    }

    pub fn back(&mut self) -> Option<Load> {
        let tab = &mut self.tabs[self.active];
        if tab.index == 0 || tab.history.is_empty() {
            self.toast(text::NO_BACK);
            return None;
        }
        tab.index -= 1;
        let url = tab.history[tab.index].clone();
        Some(self.start_load(self.active, url, false))
    }

    pub fn forward(&mut self) -> Option<Load> {
        let tab = &mut self.tabs[self.active];
        if tab.index + 1 >= tab.history.len() {
            self.toast(text::NO_FORWARD);
            return None;
        }
        tab.index += 1;
        let url = tab.history[tab.index].clone();
        Some(self.start_load(self.active, url, false))
    }

    pub fn reload(&mut self) -> Option<Load> {
        let url = self.tab().url().cloned()?;
        Some(self.start_load(self.active, url, false))
    }

    /// A worker finished. Results for a tab that is gone, or for a navigation
    /// that was superseded, are dropped.
    pub fn finish(&mut self, tab_id: u64, nav: u64, result: Result<(Loaded, Image), String>) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else { return };
        if tab.loading.as_ref().map(|p| p.nav) != Some(nav) {
            return;
        }
        let pending = tab.loading.take().expect("checked above");
        if pending.push {
            if !tab.history.is_empty() {
                tab.history.truncate(tab.index + 1);
            }
            tab.history.push(pending.url.clone());
            tab.index = tab.history.len() - 1;
        }
        tab.generation += 1;
        tab.scroll = 0.0;
        match result {
            Ok((loaded, thumb)) => {
                tab.loaded = Some(loaded);
                tab.thumb = Some(thumb);
                tab.failure = None;
                tab.live_nav = nav;
            }
            Err(reason) => {
                tab.loaded = None;
                tab.thumb = None;
                tab.failure = Some(Failure { url: pending.url, reason });
            }
        }
    }

    // ----- tabs -----------------------------------------------------------

    pub fn new_tab(&mut self) {
        self.active = self.push_tab();
        self.reveal_active();
        self.begin_editing(false);
    }

    pub fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.push_tab();
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.clamp_offset();
        self.hover = Hit::Nothing;
    }

    pub fn close_active(&mut self) {
        self.close_tab(self.active);
    }

    pub fn select(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.editing = false;
            self.reveal_active();
        }
    }

    /// Close every tab and forget all history. Everything lives in memory
    /// only, so dropping it is the whole job.
    pub fn burn(&mut self) {
        self.tabs.clear();
        self.active = self.push_tab();
        self.offset = 0;
        self.editing = false;
        self.input.clear();
        self.hover = Hit::Nothing;
        self.toast(text::BURNED);
    }

    fn max_offset(&self) -> usize {
        self.tabs.len().saturating_sub(self.geom().visible)
    }

    fn clamp_offset(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }

    fn reveal_active(&mut self) {
        let visible = self.geom().visible;
        if self.active < self.offset {
            self.offset = self.active;
        } else if self.active >= self.offset + visible {
            self.offset = self.active + 1 - visible;
        }
        self.clamp_offset();
    }

    pub fn page_by(&mut self, step: isize) {
        let target = (self.offset as isize + step).clamp(0, self.max_offset() as isize);
        self.offset = target as usize;
        self.hover = Hit::Nothing;
    }

    /// Wheel / two-finger scroll over the dock moves the carousel one card per notch.
    pub fn carousel_wheel(&mut self, delta: f32) {
        self.wheel_acc += delta;
        while self.wheel_acc.abs() >= 40.0 {
            let step = self.wheel_acc.signum();
            self.wheel_acc -= step * 40.0;
            self.page_by(step as isize);
        }
    }

    // ----- address field ----------------------------------------------------

    pub fn begin_editing(&mut self, prefill: bool) {
        self.editing = true;
        self.input = if prefill { self.tab().url().map(|u| u.to_string()).unwrap_or_default() } else { String::new() };
        self.replace_input = prefill && !self.input.is_empty();
    }

    pub fn type_text(&mut self, typed: &str) {
        if !self.editing {
            return;
        }
        if self.replace_input {
            self.input.clear();
            self.replace_input = false;
        }
        self.input.extend(typed.chars().filter(|c| !c.is_control()));
    }

    pub fn backspace(&mut self) {
        if self.replace_input {
            self.input.clear();
            self.replace_input = false;
        } else {
            self.input.pop();
        }
    }

    // ----- scrolling and gestures -------------------------------------------

    pub fn active_scroll(&self) -> f32 {
        self.tab().scroll
    }

    pub fn max_scroll(&self) -> f32 {
        let g = self.geom();
        match &self.tab().loaded {
            // The dock floats over the page: let the last line scroll clear of it.
            Some(loaded) => {
                let clearance = theme::DOCK_HEIGHT + theme::DOCK_MARGIN * 2.0;
                (loaded.page.height + clearance - g.viewport.h).max(0.0)
            }
            None => 0.0,
        }
    }

    pub fn scroll_by(&mut self, delta: f32) {
        let max = self.max_scroll();
        let tab = &mut self.tabs[self.active];
        tab.scroll = (tab.scroll + delta).clamp(0.0, max);
    }

    pub fn scroll_to(&mut self, value: f32) {
        let max = self.max_scroll();
        self.tabs[self.active].scroll = value.clamp(0.0, max);
    }

    /// Feed one trackpad scroll event over the page (logical px, natural
    /// direction: positive dy pulls content down).
    pub fn gesture(&mut self, dx: f32, dy: f32) {
        self.gesture_active = true;
        let horizontal = dx.abs() > dy.abs() * 1.5;
        if horizontal || self.swipe.abs() > 0.05 {
            self.swipe = (self.swipe + dx / 150.0).clamp(-1.4, 1.4);
        } else if self.tab().scroll <= 0.0 && (dy > 0.0 || self.pull > 0.0) {
            self.pull = (self.pull + dy / 120.0).clamp(0.0, 1.4);
        } else {
            self.scroll_by(-dy);
        }
    }

    /// The fingers lifted: commit a swipe or a pull that went far enough.
    pub fn gesture_end(&mut self) -> Option<Load> {
        self.gesture_active = false;
        let (swipe, pull) = (self.swipe, self.pull);
        if swipe >= 1.0 {
            self.swipe = 0.0;
            return self.back();
        }
        if swipe <= -1.0 {
            self.swipe = 0.0;
            return self.forward();
        }
        if pull >= 1.0 {
            self.pull = 0.0;
            return self.reload();
        }
        None
    }

    pub fn set_link(&mut self, href: Option<String>) {
        self.link = href;
    }

    pub fn active_page(&self) -> Option<(u64, u64, f32, &Page)> {
        let tab = self.tab();
        if tab.failure.is_some() {
            return None;
        }
        tab.loaded.as_ref().map(|l| (tab.id, tab.generation, tab.scroll, &l.page))
    }

    /// The active tab's shown page and the navigation it came from.
    pub fn active_live(&self) -> Option<(u64, u64)> {
        let tab = self.tab();
        (tab.loaded.is_some() && tab.failure.is_none()).then_some((tab.id, tab.live_nav))
    }

    pub fn has_tab(&self, id: u64) -> bool {
        self.tabs.iter().any(|t| t.id == id)
    }

    /// A script changed the page: show its new layout (scroll stays).
    pub fn update_page(&mut self, tab_id: u64, nav: u64, page: Option<Page>, title: Option<String>) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id && t.live_nav == nav) else { return false };
        let Some(loaded) = tab.loaded.as_mut() else { return false };
        if let Some(title) = title {
            loaded.title = title;
        }
        if let Some(page) = page {
            loaded.page = page;
            tab.generation += 1;
            let max = (loaded.page.height + theme::DOCK_HEIGHT + theme::DOCK_MARGIN * 2.0).max(0.0);
            tab.scroll = tab.scroll.min(max);
        }
        true
    }

    pub fn active_loaded(&self) -> Option<&Loaded> {
        let tab = self.tab();
        tab.loaded.as_ref().filter(|_| tab.failure.is_none())
    }

    // ----- animation --------------------------------------------------------

    /// Advance every animation by `dt` seconds; true while anything still moves.
    pub fn tick(&mut self, dt: f32) -> bool {
        let dt = dt.clamp(0.0, 0.1);
        let mut moving = false;
        let open = self.editing || self.hover == Hit::Capsule;
        moving |= ease(&mut self.addr_open, if open { 1.0 } else { 0.0 }, dt * 0.75);
        moving |= ease(&mut self.offset_anim, self.offset as f32, dt);
        if !self.gesture_active {
            moving |= ease(&mut self.swipe, 0.0, dt);
            moving |= ease(&mut self.pull, 0.0, dt);
        }
        let hovered = match self.hover {
            Hit::Tab(i) => Some(i),
            _ => None,
        };
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let target = card_target(i, hovered, i == active);
            let a = &mut tab.anim;
            moving |= ease(&mut a.push, target.push, dt);
            moving |= ease(&mut a.lift, target.lift, dt);
            moving |= ease(&mut a.tilt, target.tilt, dt);
            moving |= ease(&mut a.scale, target.scale, dt);
            moving |= ease(&mut a.depth, target.depth, dt);
            moving |= ease(&mut a.hot, target.hot, dt);
        }
        if let Some((_, left)) = &mut self.toast {
            *left -= dt;
            moving = true;
            if *left <= 0.0 {
                self.toast = None;
            }
        }
        if self.tabs.iter().any(|t| t.loading.is_some()) {
            self.spin += dt;
            moving = true;
        }
        moving || self.gesture_active
    }
}

fn card_target(i: usize, hovered: Option<usize>, active: bool) -> CardAnim {
    let rest_lift = if active { 5.0 } else { 0.0 };
    match hovered {
        None => CardAnim { tilt: theme::TILT_DEG, lift: rest_lift, scale: 1.0, ..CardAnim::default() },
        Some(h) if h == i => CardAnim {
            push: 0.0,
            lift: theme::LIFT,
            tilt: 0.0,
            scale: theme::MAGNIFY,
            depth: 60.0,
            hot: 1.0,
        },
        Some(h) => {
            let d = i as isize - h as isize;
            let near = d.abs() == 1;
            let side = d.signum() as f32;
            CardAnim {
                push: side * if near { 26.0 } else { 14.0 },
                lift: if near { 8.0 } else { rest_lift },
                tilt: -side * (theme::TILT_DEG + 8.0),
                scale: if near { 1.1 } else { 1.0 },
                depth: 0.0,
                hot: 0.0,
            }
        }
    }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b), a: m(a.a, b.a) }
}

fn alpha(c: Color, a: f32) -> Color {
    Color { a: (c.a as f32 * a.clamp(0.0, 1.0)).round() as u8, ..c }
}

// ---------------------------------------------------------------------------
// Painter: anti-aliased primitives in logical px over a device-px canvas.
// ---------------------------------------------------------------------------

struct Painter<'a> {
    c: &'a mut Canvas,
    s: f32,
}

fn sd_round_rect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let r = r.min(hw).min(hh).max(0.0);
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// A rounded rectangle in device px, with the row-span queries the fast
/// fill paths need.
struct RoundShape {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    rad: f32,
}

impl RoundShape {
    fn new(r: Rect, radius: f32, s: f32) -> RoundShape {
        let (w, h) = (r.w * s, r.h * s);
        RoundShape {
            left: r.x * s,
            right: r.x * s + w,
            top: r.y * s,
            bottom: r.y * s + h,
            rad: (radius * s).min(w / 2.0).min(h / 2.0).max(0.0),
        }
    }

    fn sd(&self, x: i32, y: i32) -> f32 {
        let (hw, hh) = ((self.right - self.left) / 2.0, (self.bottom - self.top) / 2.0);
        sd_round_rect(x as f32 + 0.5, y as f32 + 0.5, self.left + hw, self.top + hh, hw, hh, self.rad)
    }

    /// Pixel bounds (x0, x1, y0, y1), grown by `pad` device px.
    fn bounds(&self, pad: f32) -> (i32, i32, i32, i32) {
        (
            (self.left - pad).floor() as i32,
            (self.right + pad).ceil() as i32,
            (self.top - pad).floor() as i32,
            (self.bottom + pad).ceil() as i32,
        )
    }

    /// Pixels `xa..xb` of row `y` that lie wholly inside the shape shrunk
    /// by `inset` device px; None when the row has none.
    fn inner_span(&self, y: i32, inset: f32) -> Option<(i32, i32)> {
        let (top, bottom) = (self.top + inset, self.bottom - inset);
        let (left, right) = (self.left + inset, self.right - inset);
        let rad = (self.rad - inset).max(0.0);
        let (ya, yb) = (y as f32, y as f32 + 1.0);
        if ya < top || yb > bottom {
            return None;
        }
        // how far the corner arcs pull the edge in at height `yy`
        let pull = |yy: f32| {
            let dy = if yy < top + rad {
                top + rad - yy
            } else if yy > bottom - rad {
                yy - (bottom - rad)
            } else {
                return 0.0;
            };
            rad - (rad * rad - dy.min(rad).powi(2)).max(0.0).sqrt()
        };
        let dx = pull(ya).max(pull(yb));
        let (xa, xb) = ((left + dx).ceil() as i32, (right - dx).floor() as i32);
        (xa < xb).then_some((xa, xb))
    }
}

/// One horizontal and one vertical box-blur pass of `radius`, edges clamped.
fn box_blur(buf: &mut [[f32; 3]], w: usize, h: usize, radius: usize) {
    let mut line = vec![[0.0f32; 3]; w.max(h)];
    let span = (2 * radius + 1) as f32;
    let mut run = |get: &dyn Fn(usize) -> [f32; 3], n: usize, out: &mut [[f32; 3]]| {
        let at = |i: isize| get(i.clamp(0, n as isize - 1) as usize);
        let mut sum = [0.0f32; 3];
        for k in -(radius as isize)..=(radius as isize) {
            let v = at(k);
            for c in 0..3 {
                sum[c] += v[c];
            }
        }
        for i in 0..n {
            out[i] = [sum[0] / span, sum[1] / span, sum[2] / span];
            let (add, sub) = (at(i as isize + radius as isize + 1), at(i as isize - radius as isize));
            for c in 0..3 {
                sum[c] += add[c] - sub[c];
            }
        }
    };
    for y in 0..h {
        let row: Vec<[f32; 3]> = buf[y * w..(y + 1) * w].to_vec();
        run(&|i| row[i], w, &mut line[..w]);
        buf[y * w..(y + 1) * w].copy_from_slice(&line[..w]);
    }
    for x in 0..w {
        let col: Vec<[f32; 3]> = (0..h).map(|y| buf[y * w + x]).collect();
        run(&|i| col[i], h, &mut line[..h]);
        for y in 0..h {
            buf[y * w + x] = line[y];
        }
    }
}

fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (pax, pay, bax, bay) = (px - ax, py - ay, bx - ax, by - ay);
    let len = bax * bax + bay * bay;
    let h = if len > 0.0 { ((pax * bax + pay * bay) / len).clamp(0.0, 1.0) } else { 0.0 };
    ((pax - bax * h).powi(2) + (pay - bay * h).powi(2)).sqrt()
}

impl<'a> Painter<'a> {
    fn put(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x as usize >= self.c.width || y as usize >= self.c.height {
            return;
        }
        let a = (coverage * color.a as f32 / 255.0).clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = (y as usize * self.c.width + x as usize) * 4;
        let src = [color.r as f32, color.g as f32, color.b as f32];
        for ch in 0..3 {
            let dst = self.c.pixels[i + ch] as f32;
            self.c.pixels[i + ch] = (src[ch] * a + dst * (1.0 - a)).round() as u8;
        }
        self.c.pixels[i + 3] = 255;
    }

    fn fill(&mut self, color: Color) {
        for px in self.c.pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[color.r, color.g, color.b, 255]);
        }
    }

    /// Fill pixels `xa..xb` of row `y` at full coverage: a straight copy for
    /// opaque colours, a blend otherwise. The fast path for shape interiors.
    fn span(&mut self, y: i32, xa: i32, xb: i32, color: Color) {
        if y < 0 || y as usize >= self.c.height {
            return;
        }
        let (xa, xb) = (xa.max(0) as usize, (xb.max(0) as usize).min(self.c.width));
        if xa >= xb {
            return;
        }
        let row = y as usize * self.c.width;
        if color.a == 255 {
            let px = [color.r, color.g, color.b, 255];
            for chunk in self.c.pixels[(row + xa) * 4..(row + xb) * 4].chunks_exact_mut(4) {
                chunk.copy_from_slice(&px);
            }
        } else {
            for x in xa..xb {
                self.put(x as i32, y, color, 1.0);
            }
        }
    }

    fn round_rect(&mut self, r: Rect, radius: f32, color: Color) {
        let shape = RoundShape::new(r, radius, self.s);
        let (x0, x1, y0, y1) = shape.bounds(0.0);
        for y in y0..y1 {
            // Interior pixels need no distance maths: copy them in one run and
            // evaluate the SDF only on the anti-aliased rims.
            let (ia, ib) = shape.inner_span(y, 0.0).unwrap_or((x1, x1));
            for x in (x0..ia.min(x1)).chain(ib.max(x0)..x1) {
                let cov = (0.5 - shape.sd(x, y)).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, color, cov);
                }
            }
            if ia < ib {
                self.span(y, ia, ib, color);
            }
        }
    }

    fn round_stroke(&mut self, r: Rect, radius: f32, width: f32, color: Color) {
        let shape = RoundShape::new(r, radius, self.s);
        let half = width * self.s / 2.0;
        let (x0, x1, y0, y1) = shape.bounds(1.0);
        for y in y0..y1 {
            // everything deeper than the line is untouched
            let (ia, ib) = shape.inner_span(y, half * 2.0 + 1.0).unwrap_or((x1, x1));
            for x in (x0..ia.min(x1)).chain(ib.max(x0)..x1) {
                // stroke centred half a line inside the edge
                let d = shape.sd(x, y) + half;
                let cov = (half + 0.5 - d.abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, color, cov);
                }
            }
        }
    }

    /// A soft drop shadow under `r`. Only the part outside `r` is painted:
    /// every caller covers `r` itself with an opaque panel right after.
    fn shadow(&mut self, r: Rect, radius: f32, blur: f32, offset_y: f32, strength: f32) {
        let s = self.s;
        let cover = RoundShape::new(r, radius, s);
        let shape = RoundShape::new(Rect { y: r.y + offset_y, ..r }, radius, s);
        let b = blur * s;
        let (x0, x1, y0, y1) = shape.bounds(blur * s);
        let black = rgb(0, 0, 0);
        for y in y0..y1 {
            let (ia, ib) = cover.inner_span(y, 1.0).unwrap_or((x1, x1));
            for x in (x0..ia.min(x1)).chain(ib.max(x0)..x1) {
                let d = shape.sd(x, y);
                let t = 1.0 - smooth((d + b * 0.25) / b);
                if t > 0.0 {
                    self.put(x, y, black, t * strength);
                }
            }
        }
    }

    /// Blur what is already drawn under the rounded rect `r` and lay a tint
    /// over it: a frosted-glass panel. Three box-blur passes ≈ a Gaussian.
    fn frosted(&mut self, r: Rect, radius: f32, tint: Color, blur: f32) {
        let s = self.s;
        let reach = (blur * s).ceil() as i32;
        let (cw, ch) = (self.c.width as i32, self.c.height as i32);
        let x0 = ((r.x * s).floor() as i32 - reach).max(0);
        let x1 = (((r.x + r.w) * s).ceil() as i32 + reach).min(cw);
        let y0 = ((r.y * s).floor() as i32 - reach).max(0);
        let y1 = (((r.y + r.h) * s).ceil() as i32 + reach).min(ch);
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        if w == 0 || h == 0 {
            return;
        }
        let mut buf = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = ((y0 as usize + y) * self.c.width + x0 as usize + x) * 4;
                buf[y * w + x] = [self.c.pixels[i] as f32, self.c.pixels[i + 1] as f32, self.c.pixels[i + 2] as f32];
            }
        }
        let pass = ((blur * s) / 3.0).round().max(1.0) as usize;
        for _ in 0..3 {
            box_blur(&mut buf, w, h, pass);
        }
        let shape = RoundShape::new(r, radius, s);
        let ta = tint.a as f32 / 255.0;
        let tc = [tint.r as f32, tint.g as f32, tint.b as f32];
        for y in y0..y1 {
            for x in x0..x1 {
                let cov = (0.5 - shape.sd(x, y)).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let b = buf[(y - y0) as usize * w + (x - x0) as usize];
                let i = (y as usize * self.c.width + x as usize) * 4;
                for c in 0..3 {
                    let glass = b[c] * (1.0 - ta) + tc[c] * ta;
                    let under = self.c.pixels[i + c] as f32;
                    self.c.pixels[i + c] = (glass * cov + under * (1.0 - cov)).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        self.round_rect(Rect { x: cx - r, y: cy - r, w: r * 2.0, h: r * 2.0 }, r, color);
    }

    /// Stroke a polyline with round caps; `points` in logical px.
    fn polyline(&mut self, points: &[(f32, f32)], width: f32, color: Color) {
        let s = self.s;
        let half = width * s / 2.0;
        if points.is_empty() {
            return;
        }
        let pts: Vec<(f32, f32)> = points.iter().map(|(x, y)| (x * s, y * s)).collect();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in &pts {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            x1 = x1.max(*x);
            y1 = y1.max(*y);
        }
        let pad = half + 1.0;
        for y in (y0 - pad).floor() as i32..(y1 + pad).ceil() as i32 {
            for x in (x0 - pad).floor() as i32..(x1 + pad).ceil() as i32 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut d = f32::MAX;
                if pts.len() == 1 {
                    d = sd_segment(px, py, pts[0].0, pts[0].1, pts[0].0, pts[0].1);
                }
                for seg in pts.windows(2) {
                    d = d.min(sd_segment(px, py, seg[0].0, seg[0].1, seg[1].0, seg[1].1));
                }
                let cov = (half + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, color, cov);
                }
            }
        }
    }

    fn arc(&mut self, cx: f32, cy: f32, r: f32, from: f32, sweep: f32, width: f32, color: Color) {
        let steps = ((sweep.abs() * r / 3.0).ceil() as usize).clamp(2, 96);
        let points: Vec<(f32, f32)> = (0..=steps)
            .map(|i| {
                let a = from + sweep * i as f32 / steps as f32;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.polyline(&points, width, color);
    }

    /// A 24-unit icon drawn into a `size` box centred at (cx, cy).
    fn icon(&mut self, icon: Icon, cx: f32, cy: f32, size: f32, color: Color) {
        let k = size / 24.0;
        let map = |pts: &[(f32, f32)]| -> Vec<(f32, f32)> {
            pts.iter().map(|(x, y)| (cx + (x - 12.0) * k, cy + (y - 12.0) * k)).collect()
        };
        let w = 1.8 * k;
        match icon {
            Icon::Plus => {
                self.polyline(&map(&[(12.0, 5.0), (12.0, 19.0)]), w, color);
                self.polyline(&map(&[(5.0, 12.0), (19.0, 12.0)]), w, color);
            }
            Icon::Left => self.polyline(&map(&[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)]), w * 1.1, color),
            Icon::Right => self.polyline(&map(&[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)]), w * 1.1, color),
            Icon::Flame => self.polyline(
                &map(&[
                    (12.0, 3.0), (13.6, 6.2), (16.2, 8.8), (17.0, 12.0), (17.0, 14.2), (16.0, 17.0),
                    (13.8, 19.4), (12.0, 20.0), (10.2, 19.6), (8.4, 18.4), (7.3, 16.4), (7.0, 14.0),
                    (7.7, 11.6), (9.0, 9.8), (9.6, 11.6), (10.8, 12.6), (10.9, 9.4), (11.4, 6.0), (12.0, 3.0),
                ]),
                w,
                color,
            ),
            Icon::Shield => self.polyline(
                &map(&[
                    (12.0, 3.0), (20.0, 6.0), (20.0, 12.0), (18.8, 15.6), (16.4, 18.6), (12.0, 21.0),
                    (7.6, 18.6), (5.2, 15.6), (4.0, 12.0), (4.0, 6.0), (12.0, 3.0),
                ]),
                w,
                color,
            ),
            Icon::Alert => {
                self.polyline(&map(&[(12.0, 6.5), (12.0, 13.5)]), w * 1.3, color);
                self.polyline(&map(&[(12.0, 17.5)]), w * 1.6, color);
            }
            Icon::Reload => {
                self.arc(cx, cy, 7.0 * k, -0.6, 5.2, w, color);
                let (ex, ey) = (cx + 7.0 * k * (-0.6f32).cos(), cy + 7.0 * k * (-0.6f32).sin());
                self.polyline(&[(ex - 4.0 * k, ey - 1.0 * k), (ex, ey), (ex + 0.6 * k, ey - 4.2 * k)], w, color);
            }
        }
    }

    fn text(&mut self, fonts: &Fonts, s: &str, x: f32, baseline: f32, size: f32, color: Color, bold: bool) {
        if color.a == 0 || s.is_empty() {
            return;
        }
        let k = self.s;
        self.c.draw_text(fonts, s, x * k, baseline * k, size * k, color, bold, false, 0.0);
    }

    fn text_centered(&mut self, fonts: &Fonts, s: &str, cx: f32, baseline: f32, size: f32, color: Color, bold: bool) {
        let w = fonts.advance(s, size, bold, false);
        self.text(fonts, s, cx - w / 2.0, baseline, size, color, bold);
    }

    /// Copy `rows` rows of `src`, starting at `src_row`, to (x, y).
    fn blit_rows(&mut self, src: &Canvas, x: f32, y: f32, src_row: usize, rows: usize) {
        let (ox, oy) = ((x * self.s).round() as isize, (y * self.s).round() as isize);
        for r in 0..rows {
            let sy = src_row + r;
            let dy = oy + r as isize;
            if sy >= src.height || dy < 0 || dy as usize >= self.c.height {
                continue;
            }
            let x_start = ox.max(0) as usize;
            let x_end = ((ox + src.width as isize).max(0) as usize).min(self.c.width);
            if x_start >= x_end {
                continue;
            }
            let src_from = (sy * src.width + (x_start as isize - ox) as usize) * 4;
            let dst_from = (dy as usize * self.c.width + x_start) * 4;
            let n = (x_end - x_start) * 4;
            self.c.pixels[dst_from..dst_from + n].copy_from_slice(&src.pixels[src_from..src_from + n]);
        }
    }

    #[allow(dead_code)]
    fn blit(&mut self, src: &Canvas, x: f32, y: f32) {
        let (ox, oy) = ((x * self.s).round() as isize, (y * self.s).round() as isize);
        for sy in 0..src.height {
            let dy = oy + sy as isize;
            if dy < 0 || dy as usize >= self.c.height {
                continue;
            }
            let x_start = ox.max(0) as usize;
            let x_end = ((ox + src.width as isize) as usize).min(self.c.width);
            if x_start >= x_end {
                continue;
            }
            let src_from = (sy * src.width + (x_start as isize - ox) as usize) * 4;
            let dst_from = (dy as usize * self.c.width + x_start) * 4;
            let n = (x_end - x_start) * 4;
            self.c.pixels[dst_from..dst_from + n].copy_from_slice(&src.pixels[src_from..src_from + n]);
        }
    }

    /// Paint `color` over everything outside the rounded rect `r`, but only in
    /// its four corner squares: turns a blitted square page into a card.
    fn mask_corners(&mut self, r: Rect, radius: f32, color: Color) {
        let s = self.s;
        let rad = radius * s;
        let (cx, cy, hw, hh) = ((r.x + r.w / 2.0) * s, (r.y + r.h / 2.0) * s, r.w * s / 2.0, r.h * s / 2.0);
        let corners = [
            (r.x * s, r.y * s),
            ((r.x + r.w) * s - rad, r.y * s),
            (r.x * s, (r.y + r.h) * s - rad),
            ((r.x + r.w) * s - rad, (r.y + r.h) * s - rad),
        ];
        for (x0, y0) in corners {
            for y in y0.floor() as i32..(y0 + rad).ceil() as i32 {
                for x in x0.floor() as i32..(x0 + rad).ceil() as i32 {
                    let d = sd_round_rect(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, rad);
                    let outside = (d + 0.5).clamp(0.0, 1.0);
                    if outside > 0.0 {
                        self.put(x, y, color, outside);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Plus,
    Left,
    Right,
    Flame,
    Shield,
    Alert,
    Reload,
}

/// What a tab card shows on its face.
enum Face<'a> {
    Thumb(&'a Image),
    Blank(Color),
}

struct CardPose {
    cx: f32,
    bottom: f32,
    w: f32,
    h: f32,
    tilt_deg: f32,
    depth: f32,
    bright: f32,
    border: Color,
    clip: (f32, f32),
}

impl Painter<'_> {
    /// A card rotated about its vertical axis, in true perspective: each screen
    /// column is mapped back to card space (the inverse of the projection),
    /// then sampled; edges are anti-aliased through a rounded-rect distance.
    fn card(&mut self, pose: &CardPose, face: &Face) {
        let s = self.s;
        let (cx, by, w, h) = (pose.cx * s, pose.bottom * s, pose.w * s, pose.h * s);
        let (tz, d) = (pose.depth * s, theme::PERSPECTIVE * s);
        let (sin, cos) = pose.tilt_deg.to_radians().sin_cos();
        let radius = 0.13 * w.min(h);
        let border = 1.0 * s;
        let project = |u: f32| {
            let k = d / (d - tz - u * sin);
            (cx + u * cos * k, k)
        };
        let (xl, kl) = project(-w / 2.0);
        let (xr, kr) = project(w / 2.0);
        let kmax = kl.max(kr).max(0.0);
        let x0 = (xl.min(xr).max(pose.clip.0 * s)).floor() as i32;
        let x1 = (xl.max(xr).min(pose.clip.1 * s)).ceil() as i32;
        let y0 = (by - h * kmax).floor() as i32;
        let y1 = by.ceil() as i32 + 1;
        for x in x0..x1 {
            let dx = x as f32 + 0.5 - cx;
            let den = cos * d + dx * sin;
            if den.abs() < 1e-3 {
                continue;
            }
            let u = dx * (d - tz) / den;
            if u.abs() > w / 2.0 + 2.0 {
                continue;
            }
            let k = d / (d - tz - u * sin);
            if k <= 0.0 {
                continue;
            }
            // local px per screen px across this column (foreshortened)
            let px_scale = k * cos.abs().max(0.15);
            for y in y0..y1 {
                let ly = (y as f32 + 0.5 - by) / k; // -h..0
                let sd = sd_round_rect(u, ly + h / 2.0, 0.0, 0.0, w / 2.0, h / 2.0, radius);
                let cov = (0.5 - sd * px_scale.min(k)).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let (tu, tv) = (u / w + 0.5, ly / h + 1.0);
                let base = match face {
                    Face::Thumb(img) => img.sample(tu, tv),
                    Face::Blank(tint) => blank_face(*tint, tu, tv),
                };
                let mut c = rgb(
                    (base[0] * pose.bright).min(255.0) as u8,
                    (base[1] * pose.bright).min(255.0) as u8,
                    (base[2] * pose.bright).min(255.0) as u8,
                );
                let edge = ((sd + border) / border).clamp(0.0, 1.0);
                if edge > 0.0 {
                    c = mix(c, pose.border, edge);
                }
                self.put(x, y, c, cov);
            }
        }
    }
}

/// The face of a tab with nothing to show yet: its tint, a title bar and three lines.
fn blank_face(tint: Color, u: f32, v: f32) -> [f32; 3] {
    let base = [tint.r as f32, tint.g as f32, tint.b as f32];
    let lighten = |a: f32| [base[0] + (255.0 - base[0]) * a, base[1] + (255.0 - base[1]) * a, base[2] + (255.0 - base[2]) * a];
    if v < 0.18 {
        return lighten(0.16);
    }
    let bar = |v0: f32, v1: f32, u1: f32| v >= v0 && v <= v1 && u >= 0.12 && u <= u1;
    if bar(0.32, 0.41, 0.64) {
        lighten(0.34)
    } else if bar(0.50, 0.56, 0.88) || bar(0.63, 0.69, 0.74) {
        lighten(0.16)
    } else {
        base
    }
}

fn fit(fonts: &Fonts, s: &str, size: f32, bold: bool, max_w: f32) -> String {
    if fonts.advance(s, size, bold, false) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if fonts.advance(&candidate, size, bold, false) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "…"
}

fn wrap(fonts: &Fonts, s: &str, size: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if fonts.advance(&candidate, size, false, false) > max_w && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(fit(fonts, &(last + " …"), size, false, max_w));
    }
    lines
}

// ---------------------------------------------------------------------------
// Drawing the window
// ---------------------------------------------------------------------------

impl Ui {
    /// Draw the whole window into `canvas` (device px; `scale` = device px per
    /// logical px). `page_view` is the active page already painted (a band of
    /// it, and the device row in that band where the viewport starts) at the
    /// viewport's device size.
    pub fn draw(&self, canvas: &mut Canvas, fonts: &Fonts, page_view: Option<(&Canvas, usize)>, scale: f32) {
        let g = self.geom();
        let mut p = Painter { c: canvas, s: scale };
        p.fill(theme::WINDOW);
        self.draw_viewport(&mut p, fonts, &g, page_view);
        self.draw_link(&mut p, fonts, &g);
        self.draw_dock(&mut p, fonts, &g);
        self.draw_capsule(&mut p, fonts, &g);
        self.draw_gestures(&mut p, &g);
        self.draw_toast(&mut p, fonts, &g);
    }

    fn draw_viewport(&self, p: &mut Painter, fonts: &Fonts, g: &Geom, page_view: Option<(&Canvas, usize)>) {
        let v = g.viewport;
        let tab = self.tab();
        match (page_view, &tab.failure) {
            (Some((view, offset)), None) => {
                let rows = (v.h * p.s).round() as usize;
                p.blit_rows(view, v.x, v.y, offset, rows);
                p.mask_corners(v, theme::VIEW_RADIUS, theme::WINDOW);
            }
            (_, Some(failure)) => {
                p.round_rect(v, theme::VIEW_RADIUS, theme::PANEL);
                self.draw_failure(p, fonts, &v, failure);
            }
            _ => {
                p.round_rect(v, theme::VIEW_RADIUS, theme::PANEL);
                if let Some(pending) = &tab.loading {
                    let label = format!("{} {}…", text::LOADING, pending.url.host);
                    p.text_centered(fonts, &label, v.x + v.w / 2.0, v.y + v.h * 0.42, 15.0, theme::TEXT_DIM, false);
                } else {
                    self.draw_start(p, fonts, &v);
                }
            }
        }
        if tab.loading.is_some() {
            // indeterminate progress along the top edge of the page
            let span = v.w * 0.28;
            let travel = v.w + span;
            let x = v.x - span + (self.spin * 0.9 % 1.0) * travel;
            let x0 = x.max(v.x + theme::VIEW_RADIUS);
            let x1 = (x + span).min(v.x + v.w - theme::VIEW_RADIUS);
            if x1 > x0 {
                p.round_rect(Rect { x: x0, y: v.y, w: x1 - x0, h: 2.5 }, 1.25, theme::ACCENT);
            }
        }
    }

    fn draw_start(&self, p: &mut Painter, fonts: &Fonts, v: &Rect) {
        let col = (v.w - 48.0).min(640.0);
        let left = v.x + (v.w - col) / 2.0;
        let mut y = v.y + (v.h * 0.22).max(56.0);
        p.text_centered(fonts, text::START_TITLE, v.x + v.w / 2.0, y, 34.0, theme::TEXT, true);
        y += 34.0;
        p.text_centered(fonts, &fit(fonts, text::START_HINT, 14.0, false, col), v.x + v.w / 2.0, y, 14.0, theme::TEXT_DIM, false);
        y += 36.0;

        let route = self.transport.describe();
        let facts: [(&str, String, Color); 4] = [
            ("Маршрут", route, self.transport.color()),
            ("DNS", if self.doh { "через DoH, не системным резолвером".into() } else { "системный резолвер".into() }, if self.doh { theme::TEXT } else { theme::DIRECT }),
            ("JavaScript", "не исполняется: движка нет".into(), theme::TEXT),
            ("Куки и диск", "не хранятся: всё живёт в памяти".into(), theme::TEXT),
        ];
        let two_columns = col >= 520.0;
        let card_w = if two_columns { (col - 12.0) / 2.0 } else { col };
        for (i, (label, value, color)) in facts.iter().enumerate() {
            let (cx, cy) = if two_columns {
                (left + (i % 2) as f32 * (card_w + 12.0), y + (i / 2) as f32 * 76.0)
            } else {
                (left, y + i as f32 * 70.0)
            };
            let r = Rect { x: cx, y: cy, w: card_w, h: 64.0 };
            p.round_stroke(r, 14.0, 1.0, theme::STROKE);
            p.text(fonts, label, r.x + 16.0, r.y + 24.0, 12.0, theme::TEXT_DIM, false);
            p.text(fonts, &fit(fonts, value, 14.0, false, card_w - 32.0), r.x + 16.0, r.y + 46.0, 14.0, *color, false);
        }
        let rows = if two_columns { 2.0 } else { 4.0 };
        y += rows * if two_columns { 76.0 } else { 70.0 } + 20.0;
        for line in wrap(fonts, text::GESTURES, 13.0, col, 2) {
            p.text_centered(fonts, &line, v.x + v.w / 2.0, y, 13.0, theme::TEXT_DIM, false);
            y += 19.0;
        }
    }

    fn draw_failure(&self, p: &mut Painter, fonts: &Fonts, v: &Rect, failure: &Failure) {
        let col = (v.w - 48.0).min(560.0);
        let cx = v.x + v.w / 2.0;
        let mut y = v.y + (v.h * 0.24).max(48.0);
        p.circle(cx, y, 26.0, alpha(theme::DANGER, 0.14));
        p.icon(Icon::Alert, cx, y, 30.0, theme::DANGER);
        y += 64.0;
        p.text_centered(fonts, text::ERROR_TITLE, cx, y, 24.0, theme::TEXT, true);
        y += 28.0;
        let address = fit(fonts, &failure.url.to_string(), 13.0, false, col);
        p.text_centered(fonts, &address, cx, y, 13.0, theme::TEXT_DIM, false);
        y += 30.0;
        let box_lines = wrap(fonts, &failure.reason, 14.0, col - 32.0, 4);
        let box_h = 24.0 + box_lines.len() as f32 * 20.0;
        let r = Rect { x: cx - col / 2.0, y, w: col, h: box_h };
        p.round_rect(r, 12.0, theme::RAISED);
        let mut ly = r.y + 26.0;
        for line in &box_lines {
            p.text(fonts, line, r.x + 16.0, ly, 14.0, theme::TEXT, false);
            ly += 20.0;
        }
        y += box_h + 28.0;
        p.text_centered(fonts, &fit(fonts, text::ERROR_RETRY, 13.0, false, col), cx, y, 13.0, theme::TEXT_DIM, false);
    }

    fn draw_link(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let Some(href) = &self.link else { return };
        let room = g.dock.x - g.viewport.x - 24.0;
        let (max_w, y) = if room >= 160.0 {
            (room.min(440.0), g.viewport.y + g.viewport.h - 36.0)
        } else {
            ((g.viewport.w - 24.0).min(440.0), g.dock.y - 36.0)
        };
        let label = fit(fonts, href, 12.0, false, max_w - 20.0);
        let w = fonts.advance(&label, 12.0, false, false) + 20.0;
        let r = Rect { x: g.viewport.x + 12.0, y, w, h: 24.0 };
        p.round_rect(r, 12.0, alpha(theme::PANEL, 0.94));
        p.text(fonts, &label, r.x + 10.0, r.y + 16.0, 12.0, theme::TEXT_DIM, false);
    }

    fn draw_dock(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let d = g.dock;
        p.shadow(d, 24.0, 22.0, 10.0, 0.5);
        p.round_rect(d, 24.0, theme::PANEL);
        p.round_stroke(d, 24.0, 1.0, theme::STROKE);

        let hover_bg = |hit: Hit, base: Color| if self.hover == hit { theme::RAISED_HOVER } else { base };
        p.round_rect(g.new_tab, 14.0, hover_bg(Hit::NewTab, theme::RAISED));
        p.icon(Icon::Plus, g.new_tab.x + 22.0, g.new_tab.y + 22.0, 22.0, theme::TEXT);

        let sep = |p: &mut Painter, x: f32| p.round_rect(Rect { x, y: d.y + 30.0, w: 1.0, h: 32.0 }, 0.5, theme::STROKE);
        sep(p, g.new_tab.x + theme::BUTTON + 8.0);
        sep(p, g.chip.x - 9.0);

        if g.arrows {
            let can_prev = self.offset > 0;
            let can_next = self.offset < self.max_offset();
            let tone = |on: bool, hit: Hit| {
                if !on {
                    alpha(theme::TEXT_DIM, 0.3)
                } else if self.hover == hit {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                }
            };
            p.icon(Icon::Left, g.prev.x + g.prev.w / 2.0, g.prev.y + 22.0, 16.0, tone(can_prev, Hit::Prev));
            p.icon(Icon::Right, g.next.x + g.next.w / 2.0, g.next.y + 22.0, 16.0, tone(can_next, Hit::Next));
        }

        // transport chip
        let t = self.transport;
        p.round_rect(g.chip, 14.0, hover_bg(Hit::Transport, theme::RAISED));
        p.icon(Icon::Shield, g.chip.x + 20.0, g.chip.y + 22.0, 18.0, t.color());
        p.text(fonts, t.label(), g.chip.x + 36.0, g.chip.y + 27.0, 13.0, t.color(), true);

        // burn
        if self.hover == Hit::Burn {
            p.round_rect(g.burn, 14.0, alpha(theme::DANGER, 0.16));
        }
        p.round_stroke(g.burn, 14.0, 1.0, theme::STROKE);
        let flame = if self.hover == Hit::Burn { theme::DANGER } else { theme::ACCENT };
        p.icon(Icon::Flame, g.burn.x + 22.0, g.burn.y + 22.0, 20.0, flame);

        self.draw_cards(p, fonts, g);
    }

    fn draw_cards(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let clip = (g.strip.x + 4.0, g.strip.x + g.strip.w - 4.0);
        let hovered = match self.hover {
            Hit::Tab(i) => Some(i),
            _ => None,
        };
        // back to front: farthest from the hovered card first
        let mut order: Vec<usize> = (0..self.tabs.len()).collect();
        order.sort_by_key(|&i| {
            let far = hovered.map(|h| (h as isize - i as isize).unsigned_abs()).unwrap_or(0);
            (std::cmp::Reverse(far), (self.tabs[i].anim.hot * 100.0) as i32)
        });
        let card_bottom = g.dock.y + 12.0 + theme::CARD_H;
        let icon_y = g.dock.y + 12.0 + 68.0 - 9.0;
        let mut label: Option<(f32, f32, usize)> = None;
        for i in order {
            let tab = &self.tabs[i];
            let a = tab.anim;
            let cx = self.slot_center(g, i) + a.push;
            if cx < clip.0 - theme::SLOT || cx > clip.1 + theme::SLOT {
                continue;
            }
            let active = i == self.active;
            let pose = CardPose {
                cx,
                bottom: card_bottom - a.lift,
                w: theme::CARD_W * a.scale,
                h: theme::CARD_H * a.scale,
                tilt_deg: a.tilt,
                depth: a.depth,
                bright: 0.82 + 0.18 * if active { 1.0 } else { 0.0 } + 0.25 * a.hot,
                border: if a.hot > 0.5 || active { theme::ACCENT } else { rgb(58, 65, 76) },
                clip,
            };
            if a.hot > 0.05 {
                let w = pose.w;
                let r = Rect { x: cx - w / 2.0, y: pose.bottom - pose.h, w, h: pose.h };
                p.shadow(r, 8.0, 14.0, 10.0, 0.45 * a.hot);
            }
            let face = match &tab.thumb {
                Some(img) => Face::Thumb(img),
                None => Face::Blank(tab.tint()),
            };
            p.card(&pose, &face);

            // monogram under the card
            if cx >= clip.0 && cx <= clip.1 {
                let size = 18.0 * (1.0 + 0.2 * a.hot);
                if a.hot > 0.01 {
                    p.circle(cx, icon_y, size / 2.0 + 4.0, alpha(theme::ACCENT, 0.2 * a.hot));
                }
                let bg = mix(theme::RAISED, theme::ACCENT, a.hot);
                let fg = mix(if active { theme::ACCENT } else { rgb(195, 201, 209) }, theme::ON_ACCENT, a.hot);
                let r = Rect { x: cx - size / 2.0, y: icon_y - size / 2.0, w: size, h: size };
                p.round_rect(r, size / 3.0, bg);
                let letter = tab.monogram().to_string();
                p.text_centered(fonts, &letter, cx, icon_y + size * 0.2, size * 0.55, fg, true);
                if active {
                    p.circle(cx, g.dock.y + g.dock.h - 6.0, 2.0, theme::ACCENT);
                }
                if tab.loading.is_some() {
                    p.arc(cx, icon_y, size / 2.0 + 3.0, self.spin * 6.0, 1.6, 1.6, theme::ACCENT);
                }
            }
            if a.hot > 0.05 {
                label = Some((cx, pose.bottom - pose.h - 10.0, i));
            }
        }
        if let Some((cx, bottom, i)) = label {
            let tab = &self.tabs[i];
            let t = tab.anim.hot;
            let title = fit(fonts, &tab.title(), 13.0, true, 300.0);
            let host = tab.host();
            let tw = fonts.advance(&title, 13.0, true, false).max(fonts.advance(&host, 11.0, false, false));
            let h = if host.is_empty() { 30.0 } else { 44.0 };
            let w = tw + 24.0;
            let x = (cx - w / 2.0).clamp(8.0, self.w - w - 8.0);
            let r = Rect { x, y: bottom - h + (1.0 - t) * 8.0, w, h };
            p.shadow(r, 12.0, 12.0, 6.0, 0.4 * t);
            p.round_rect(r, 12.0, alpha(theme::RAISED, t));
            p.round_stroke(r, 12.0, 1.0, alpha(rgb(42, 47, 56), t));
            p.text_centered(fonts, &title, r.x + w / 2.0, r.y + 20.0, 13.0, alpha(theme::TEXT, t), true);
            if !host.is_empty() {
                p.text_centered(fonts, &host, r.x + w / 2.0, r.y + 35.0, 11.0, alpha(theme::TEXT_DIM, t), false);
            }
        }
    }

    fn draw_capsule(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let c = g.capsule;
        let tab = self.tab();
        // frosted glass: the page behind, blurred and tinted
        p.shadow(c, c.h / 2.0, 14.0, 4.0, 0.22);
        p.frosted(c, c.h / 2.0, alpha(theme::PANEL, 0.62), 14.0);
        p.round_stroke(c, c.h / 2.0, 1.0, Color { r: 255, g: 255, b: 255, a: 34 });

        let transport = Transport::of_tab(self.transport, tab);
        // monogram, always visible, at the left end
        let (mx, my) = (c.x + c.h / 2.0, c.y + c.h / 2.0);
        p.circle(mx, my, 11.0, tab.tint());
        p.text_centered(fonts, &tab.monogram().to_string(), mx, my + 4.0, 11.0, theme::TEXT, true);
        p.circle(mx + 8.5, my + 8.5, 4.5, theme::PANEL);
        p.circle(mx + 8.5, my + 8.5, 3.0, transport.color());
        if tab.loading.is_some() {
            p.arc(mx, my, 14.5, self.spin * 6.0, 1.8, 1.8, theme::ACCENT);
        }

        let t = ((self.addr_open - 0.35) / 0.65).clamp(0.0, 1.0);
        if t <= 0.0 {
            return;
        }
        // transport tag
        let label = transport.label();
        let tag_w = fonts.advance(label, 11.0, true, false) + 14.0;
        let tag = Rect { x: c.x + 38.0, y: my - 10.0, w: tag_w, h: 20.0 };
        p.round_rect(tag, 10.0, alpha(transport.color(), 0.18 * t));
        p.text(fonts, label, tag.x + 7.0, my + 4.0, 11.0, alpha(transport.color(), t), true);

        let text_x = tag.x + tag_w + 10.0;
        let room = c.x + c.w - 16.0 - text_x;
        if room < 24.0 {
            return;
        }
        if self.editing {
            let shown = fit_tail(fonts, &self.input, 14.0, room - 4.0);
            let color = if self.replace_input { theme::ACCENT } else { theme::TEXT };
            p.text(fonts, &shown, text_x, my + 5.0, 14.0, alpha(color, t), false);
            let caret_x = text_x + fonts.advance(&shown, 14.0, false, false) + 1.0;
            p.round_rect(Rect { x: caret_x, y: my - 8.0, w: 1.5, h: 16.0 }, 0.75, alpha(theme::ACCENT, t));
            if self.input.is_empty() {
                p.text(fonts, "Адрес сайта", text_x + 6.0, my + 5.0, 14.0, alpha(theme::TEXT_DIM, t), false);
            }
        } else {
            match tab.url() {
                Some(url) => {
                    let host = url.host.clone();
                    let rest = url.to_string();
                    let rest = rest.split_once(&host).map(|(_, r)| r.to_string()).unwrap_or_default();
                    let host_fit = fit(fonts, &host, 14.0, false, room);
                    p.text(fonts, &host_fit, text_x, my + 5.0, 14.0, alpha(theme::TEXT, t), false);
                    let used = fonts.advance(&host_fit, 14.0, false, false);
                    if host_fit == host && room - used > 30.0 && rest != "/" {
                        let tail = fit(fonts, &rest, 14.0, false, room - used);
                        p.text(fonts, &tail, text_x + used, my + 5.0, 14.0, alpha(theme::TEXT_DIM, t), false);
                    }
                }
                None => p.text(fonts, "Адрес сайта", text_x, my + 5.0, 14.0, alpha(theme::TEXT_DIM, t), false),
            }
        }
    }

    fn draw_gestures(&self, p: &mut Painter, g: &Geom) {
        let v = g.viewport;
        if self.swipe.abs() > 0.02 {
            let back = self.swipe > 0.0;
            let progress = self.swipe.abs().min(1.0);
            let ready = self.swipe.abs() >= 1.0;
            let travel = 56.0 * smooth(progress);
            let x = if back { v.x - 22.0 + travel } else { v.x + v.w + 22.0 - travel };
            let y = v.y + v.h / 2.0;
            p.shadow(Rect { x: x - 22.0, y: y - 22.0, w: 44.0, h: 44.0 }, 22.0, 12.0, 4.0, 0.4 * progress);
            p.circle(x, y, 22.0, if ready { theme::ACCENT } else { alpha(theme::RAISED, progress.max(0.6)) });
            let icon = if back { Icon::Left } else { Icon::Right };
            p.icon(icon, x, y, 20.0, if ready { theme::ON_ACCENT } else { alpha(theme::TEXT, progress) });
        }
        if self.pull > 0.02 {
            let progress = self.pull.min(1.0);
            let ready = self.pull >= 1.0;
            let (x, y) = (v.x + v.w / 2.0, v.y + 8.0 + 36.0 * smooth(progress));
            p.circle(x, y, 18.0, if ready { theme::ACCENT } else { theme::RAISED });
            let color = if ready { theme::ON_ACCENT } else { theme::TEXT };
            if ready {
                p.icon(Icon::Reload, x, y, 20.0, color);
            } else {
                p.arc(x, y, 8.0, -std::f32::consts::FRAC_PI_2, progress * 6.0, 1.8, color);
            }
        }
    }

    fn draw_toast(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let Some((message, left)) = &self.toast else { return };
        let t = (left / 0.3).min(1.0);
        let w = fonts.advance(message, 13.0, false, false) + 32.0;
        let r = Rect { x: (self.w - w) / 2.0, y: g.dock.y - 52.0, w, h: 34.0 };
        p.shadow(r, 17.0, 14.0, 6.0, 0.4 * t);
        p.round_rect(r, 17.0, alpha(theme::RAISED, t));
        p.text_centered(fonts, message, r.x + w / 2.0, r.y + 22.0, 13.0, alpha(theme::TEXT, t), false);
    }
}

impl Transport {
    fn of_tab(base: Transport, tab: &Tab) -> Transport {
        match tab.url().and_then(|u| u.needs_overlay()) {
            Some("tor") => Transport::Tor,
            Some(_) => Transport::I2p,
            None => base,
        }
    }
}

/// Keep the end of a string that is being typed visible.
fn fit_tail(fonts: &Fonts, s: &str, size: f32, max_w: f32) -> String {
    if fonts.advance(s, size, false, false) <= max_w {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut start = 0;
    while start < chars.len() {
        let candidate: String = std::iter::once('…').chain(chars[start..].iter().copied()).collect();
        if fonts.advance(&candidate, size, false, false) <= max_w {
            return candidate;
        }
        start += 1;
    }
    String::new()
}

// ---------------------------------------------------------------------------
// The OS window
// ---------------------------------------------------------------------------

struct Done {
    tab_id: u64,
    nav: u64,
    result: Result<(Loaded, Image), String>,
}

/// Messages from page threads to the window.
enum PageMsg {
    Done(Done),
    /// A page's scripts answered an event.
    Update { tab_id: u64, nav: u64, outcome: Outcome },
}

/// A text field the user is typing into: its tab, navigation, element, text.
struct Field {
    tab_id: u64,
    nav: u64,
    node: usize,
    value: String,
}

/// The active page painted as a band three viewports tall around the
/// scroll position: scrolling inside it is a copy, not a repaint.
struct PageCache {
    key: (u64, u64, u32, u32, u32),
    /// page y (logical px) of the band's first row
    top: f32,
    canvas: Canvas,
}

const BAND: f32 = 3.0;

struct App {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    fonts: Arc<Fonts>,
    chain: Chain,
    resolver: Option<Resolver>,
    proxy: EventLoopProxy<PageMsg>,
    ui: Ui,
    /// Each tab's live page: the channel to the thread holding its scripts.
    sessions: HashMap<u64, (u64, mpsc::Sender<PageEvent>)>,
    field: Option<Field>,
    frame: Canvas,
    page_cache: Option<PageCache>,
    scale: f32,
    physical: (u32, u32),
    mouse: (f32, f32),
    mods: ModifiersState,
    last_frame: Instant,
    gesture_cooldown: Instant,
    /// One display refresh: 8.3 ms on a 120 Hz panel, 16.7 ms at 60 Hz.
    frame_interval: Duration,
}

impl App {
    /// Load a page on a thread of its own. When the page has scripts the
    /// thread stays: it holds the script session (Boa contexts cannot move
    /// between threads) and answers page events until the tab moves on.
    fn spawn(&mut self, load: Load) {
        let fonts = self.fonts.clone();
        let chain = self.chain.clone();
        let resolver = self.resolver.clone();
        let proxy = self.proxy.clone();
        let width = self.ui.viewport().w;
        let (events, inbox) = mpsc::channel::<PageEvent>();
        // replacing the sender ends the previous page's thread
        self.sessions.insert(load.tab_id, (load.nav, events));
        self.field = None;
        std::thread::spawn(move || {
            // show the page as soon as HTML, CSS and scripts are in; images follow
            let opened = page::open(&load.url, &chain, resolver.as_ref(), width, &fonts, Duration::from_secs(20), false, true);
            let (result, session, late) = match opened {
                Ok((loaded, session, late)) => {
                    let thumb = thumbnail(&fonts, &loaded.page, width);
                    (Ok((loaded, thumb)), session, late)
                }
                Err(e) => (Err(e), None, None),
            };
            let _ = proxy.send_event(PageMsg::Done(Done { tab_id: load.tab_id, nav: load.nav, result }));
            let mut session = session;
            if let (Some(late), Some(session)) = (late, session.as_mut()) {
                if let Ok(images) = late.join() {
                    if !images.is_empty() {
                        session.set_images(images);
                        let page = session.layout(&fonts);
                        let outcome = Outcome { page: Some(page), ..Outcome::default() };
                        let _ = proxy.send_event(PageMsg::Update { tab_id: load.tab_id, nav: load.nav, outcome });
                    }
                }
            }
            if let Some(mut session) = session {
                while let Ok(event) = inbox.recv() {
                    let outcome = session.handle(event, &fonts);
                    if proxy.send_event(PageMsg::Update { tab_id: load.tab_id, nav: load.nav, outcome }).is_err() {
                        break;
                    }
                }
            }
        });
        self.window.request_redraw();
    }

    /// Send an event to the active page's scripts; false when it has none.
    fn to_page(&self, event: PageEvent) -> bool {
        let Some((tab_id, nav)) = self.ui.active_live() else { return false };
        match self.sessions.get(&tab_id) {
            Some((live, tx)) if *live == nav => tx.send(event).is_ok(),
            _ => false,
        }
    }

    /// The element under a point of the page: a link's `<a>`, else the
    /// smallest element box around it. (node, raw href)
    fn element_at(&self, x: f32, y: f32) -> Option<(usize, Option<String>)> {
        let loaded = self.ui.active_loaded()?;
        let v = self.ui.viewport();
        let (px, py) = (x - v.x, y - v.y + self.ui.active_scroll());
        let link = loaded.page.links.iter().find(|l| l.rect.contains(px, py));
        let boxed = loaded
            .page
            .boxes
            .iter()
            .filter(|(r, _)| r.contains(px, py))
            .min_by(|a, b| (a.0.w * a.0.h).partial_cmp(&(b.0.w * b.0.h)).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, id)| *id);
        let node = link.and_then(|l| l.node).or(boxed)?;
        Some((node, link.map(|l| l.href.clone())))
    }

    /// What the page's scripts answered.
    fn page_update(&mut self, tab_id: u64, nav: u64, outcome: Outcome) {
        if !self.ui.update_page(tab_id, nav, outcome.page, outcome.title) {
            return;
        }
        if let Some((node, value)) = outcome.focus {
            self.field = Some(Field { tab_id, nav, node, value });
        }
        if let Some(target) = outcome.navigate {
            let base = self.ui.active_loaded().map(|l| l.url.clone());
            if let Some(url) = base.and_then(|b| b.resolve(&target)).or_else(|| Url::parse(&target)) {
                let load = self.ui.navigate(url);
                self.spawn(load);
            }
        }
        self.update_title();
    }

    /// Typing into a focused page field; true when the key was used.
    fn type_into_field(&mut self, key: &Key, typed: Option<&str>) -> bool {
        let Some(field) = self.field.as_mut() else { return false };
        if self.ui.active_live() != Some((field.tab_id, field.nav)) {
            self.field = None;
            return false;
        }
        let node = field.node;
        let event = match key {
            Key::Named(NamedKey::Escape) => {
                self.field = None;
                return true;
            }
            Key::Named(NamedKey::Enter) => PageEvent::Key { node, key: "Enter".into() },
            Key::Named(NamedKey::Backspace) => {
                field.value.pop();
                PageEvent::Input { node, value: field.value.clone() }
            }
            Key::Named(NamedKey::Tab) => {
                self.field = None;
                return true;
            }
            _ => match typed.filter(|t| !t.chars().any(|c| c.is_control())) {
                Some(text) => {
                    field.value.push_str(text);
                    PageEvent::Input { node, value: field.value.clone() }
                }
                None => return true,
            },
        };
        self.to_page(event);
        true
    }

    fn run(&mut self, load: Option<Load>) {
        if let Some(load) = load {
            self.spawn(load);
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.physical = (width, height);
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            let _ = self.surface.resize(w, h);
        }
        self.ui.set_size(width as f32 / self.scale, height as f32 / self.scale);
        self.frame = Canvas::new(width.max(1) as usize, height.max(1) as usize, theme::WINDOW);
        self.page_cache = None;
    }

    fn render(&mut self) {
        let (w, h) = self.physical;
        if w == 0 || h == 0 {
            return;
        }
        let s = self.scale;
        let v = self.ui.viewport();
        let (vw, vh) = ((v.w * s).round().max(1.0) as u32, (v.h * s).round().max(1.0) as u32);
        let page_view = match self.ui.active_page() {
            Some((tab_id, generation, scroll, page)) => {
                let key = (tab_id, generation, vw, vh, s.to_bits());
                let band_h = v.h * BAND;
                let fresh = self.page_cache.as_ref().is_some_and(|c| {
                    c.key == key && scroll >= c.top && scroll + v.h <= c.top + band_h
                });
                if !fresh {
                    // repaint the band with the viewport in its middle third,
                    // reusing the bitmap when the size has not changed
                    let top = (scroll - v.h).max(0.0);
                    let band_rows = (band_h * s).round() as usize;
                    let mut canvas = match self.page_cache.take() {
                        Some(old) if old.canvas.width == vw as usize && old.canvas.height == band_rows => {
                            let mut c = old.canvas;
                            Painter { c: &mut c, s }.fill(theme::PAGE);
                            c
                        }
                        _ => Canvas::new(vw as usize, band_rows, theme::PAGE),
                    };
                    canvas.paint_scaled(&self.fonts, page, top, s);
                    self.page_cache = Some(PageCache { key, top, canvas });
                }
                self.page_cache.as_ref().map(|c| (&c.canvas, ((scroll - c.top) * s).round().max(0.0) as usize))
            }
            None => None,
        };
        self.ui.draw(&mut self.frame, &self.fonts, page_view, s);

        let Ok(mut buffer) = self.surface.buffer_mut() else { return };
        for (i, px) in self.frame.pixels.chunks_exact(4).enumerate().take(buffer.len()) {
            buffer[i] = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32;
        }
        let _ = buffer.present();
    }

    fn update_title(&self) {
        let title = match self.ui.active_loaded() {
            Some(l) if !l.title.is_empty() => format!("Sepia — {}", l.title),
            _ => "Sepia".to_string(),
        };
        self.window.set_title(&title);
    }

    fn pointer_moved(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        let hit = self.ui.hit(x, y);
        self.ui.set_hover(hit);
        let link = if hit == Hit::Page { self.link_at(x, y) } else { None };
        self.ui.set_link(link);
    }

    fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let loaded = self.ui.active_loaded()?;
        let v = self.ui.viewport();
        let (px, py) = (x - v.x, y - v.y + self.ui.active_scroll());
        loaded
            .page
            .links
            .iter()
            .find(|l| l.rect.contains(px, py))
            .and_then(|l| loaded.url.resolve(&l.href))
            .map(|u| u.to_string())
    }

    fn click(&mut self, x: f32, y: f32) {
        let hit = self.ui.hit(x, y);
        if hit != Hit::Capsule && self.ui.editing {
            self.ui.editing = false;
        }
        match hit {
            Hit::Capsule => {
                if !self.ui.editing {
                    self.ui.begin_editing(true);
                }
            }
            Hit::NewTab => self.ui.new_tab(),
            Hit::Prev => self.ui.page_by(-3),
            Hit::Next => self.ui.page_by(3),
            Hit::Tab(i) => self.ui.select(i),
            Hit::Burn => {
                self.ui.burn();
                self.page_cache = None;
                self.sessions.clear();
                self.field = None;
                // nothing of the past session stays: open connections, DNS answers
                crate::net::forget_connections();
                crate::resolver::forget();
            }
            Hit::Page => {
                // pages with scripts get the click; they decide what happens
                if let Some((node, href)) = self.element_at(x, y) {
                    self.field = None;
                    if self.to_page(PageEvent::Click { node, href }) {
                        return;
                    }
                }
                if let Some(href) = self.link_at(x, y) {
                    if let Some(url) = Url::parse(&href) {
                        let load = self.ui.navigate(url);
                        self.spawn(load);
                    }
                }
            }
            Hit::Transport | Hit::Dock | Hit::Nothing => {}
        }
        self.update_title();
    }

    fn key(&mut self, key: &Key, typed: Option<&str>) -> bool {
        let cmd = self.mods.super_key() || self.mods.control_key();
        if cmd {
            match key {
                Key::Character(c) => match c.as_str() {
                    "l" | "д" => self.ui.begin_editing(true),
                    "t" | "е" => self.ui.new_tab(),
                    "w" | "ц" => {
                        self.ui.close_active();
                        let ui = &self.ui;
                        self.sessions.retain(|id, _| ui.has_tab(*id));
                    }
                    "r" | "к" => {
                        let load = self.ui.reload();
                        self.run(load);
                    }
                    "[" | "х" => {
                        let load = self.ui.back();
                        self.run(load);
                    }
                    "]" | "ъ" => {
                        let load = self.ui.forward();
                        self.run(load);
                    }
                    "q" | "й" => return false,
                    digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                        let n = (digit.as_bytes()[0] - b'0') as usize;
                        if n >= 1 {
                            self.ui.select(n - 1);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
            self.update_title();
            return true;
        }
        if !self.ui.editing && self.type_into_field(key, typed) {
            return true;
        }
        if self.ui.editing {
            match key {
                Key::Named(NamedKey::Escape) => self.ui.editing = false,
                Key::Named(NamedKey::Enter) => {
                    let load = self.ui.submit();
                    self.run(load);
                }
                Key::Named(NamedKey::Backspace) => self.ui.backspace(),
                _ => {
                    if let Some(text) = typed {
                        self.ui.type_text(text);
                    }
                }
            }
            return true;
        }
        let page_h = self.ui.viewport().h;
        match key {
            Key::Named(NamedKey::ArrowDown) => self.ui.scroll_by(48.0),
            Key::Named(NamedKey::ArrowUp) => self.ui.scroll_by(-48.0),
            Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) => self.ui.scroll_by(page_h * 0.9),
            Key::Named(NamedKey::PageUp) => self.ui.scroll_by(-page_h * 0.9),
            Key::Named(NamedKey::Home) => self.ui.scroll_to(0.0),
            Key::Named(NamedKey::End) => self.ui.scroll_to(f32::MAX),
            Key::Character(c) if c.as_str() == "/" => self.ui.begin_editing(false),
            _ => {}
        }
        true
    }

    fn wheel(&mut self, delta: MouseScrollDelta, phase: TouchPhase) {
        let (dx, dy, precise) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * 48.0, y * 48.0, false),
            MouseScrollDelta::PixelDelta(p) => (p.x as f32 / self.scale, p.y as f32 / self.scale, true),
        };
        let (mx, my) = self.mouse;
        match self.ui.hit(mx, my) {
            Hit::Tab(_) | Hit::Dock | Hit::Prev | Hit::Next | Hit::NewTab | Hit::Transport | Hit::Burn => {
                let along = if dx.abs() > dy.abs() { -dx } else { -dy };
                self.ui.carousel_wheel(along);
            }
            _ if !precise => self.ui.scroll_by(-dy),
            _ => {
                // Momentum events keep arriving after a committed gesture;
                // ignore them until the cooldown passes.
                if Instant::now() < self.gesture_cooldown {
                    if dx.abs() < dy.abs() {
                        self.ui.scroll_by(-dy);
                    }
                    return;
                }
                match phase {
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        let load = self.ui.gesture_end();
                        if load.is_some() {
                            self.gesture_cooldown = Instant::now() + Duration::from_millis(600);
                        }
                        self.run(load);
                    }
                    _ => self.ui.gesture(dx, dy),
                }
            }
        }
        let (mx, my) = self.mouse;
        self.pointer_moved(mx, my);
    }
}

/// The refresh period of the monitor the window is on (60 Hz if unknown).
fn refresh_interval(window: &Window) -> Duration {
    let millihertz = window
        .current_monitor()
        .and_then(|m| m.refresh_rate_millihertz())
        .filter(|mhz| (24_000..=360_000).contains(mhz))
        .unwrap_or(60_000);
    Duration::from_micros(1_000_000_000 / millihertz as u64)
}

pub fn run(initial: Option<String>, chain: Chain, resolver: Option<Resolver>) -> Result<(), String> {
    let mut builder = EventLoopBuilder::<PageMsg>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        // a regular app in the Dock and the app switcher, which does not
        // force itself in front of the app you are using when it starts
        builder.with_activation_policy(ActivationPolicy::Regular).with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder.build().map_err(|e| e.to_string())?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Sepia")
            .with_inner_size(LogicalSize::new(1200.0, 860.0))
            .with_min_inner_size(LogicalSize::new(480.0, 360.0))
            .with_window_level(winit::window::WindowLevel::Normal)
            .build(&event_loop)
            .map_err(|e| e.to_string())?,
    );
    let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
    let surface = softbuffer::Surface::new(&context, window.clone()).map_err(|e| e.to_string())?;
    let transport = Transport::of(&chain, None);
    let mut app = App {
        window: window.clone(),
        surface,
        fonts: Arc::new(Fonts::load()),
        chain,
        resolver: resolver.clone(),
        proxy: event_loop.create_proxy(),
        ui: Ui::new(transport, resolver.is_some()),
        frame: Canvas::new(1, 1, theme::WINDOW),
        page_cache: None,
        scale: window.scale_factor() as f32,
        physical: (0, 0),
        mouse: (0.0, 0.0),
        mods: ModifiersState::empty(),
        last_frame: Instant::now(),
        gesture_cooldown: Instant::now(),
        frame_interval: refresh_interval(&window),
        sessions: HashMap::new(),
        field: None,
    };
    let size = window.inner_size();
    app.resize(size.width, size.height);
    if let Some(input) = initial {
        match Url::parse(&input) {
            Some(url) => {
                let load = app.ui.navigate(url);
                app.spawn(load);
            }
            None => app.ui.toast(text::BAD_ADDRESS),
        }
    }

    event_loop
        .run(move |event, target| match event {
            Event::UserEvent(PageMsg::Done(done)) => {
                app.ui.finish(done.tab_id, done.nav, done.result);
                app.page_cache = None;
                app.update_title();
                app.window.request_redraw();
            }
            Event::UserEvent(PageMsg::Update { tab_id, nav, outcome }) => {
                app.page_update(tab_id, nav, outcome);
                app.window.request_redraw();
            }
            Event::AboutToWait => {
                let dt = app.last_frame.elapsed().as_secs_f32();
                app.last_frame = Instant::now();
                if app.ui.tick(dt) {
                    app.window.request_redraw();
                    target.set_control_flow(ControlFlow::WaitUntil(Instant::now() + app.frame_interval));
                } else {
                    target.set_control_flow(ControlFlow::Wait);
                }
            }
            Event::WindowEvent { event, .. } => {
                match event {
                    WindowEvent::CloseRequested => target.exit(),
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        app.scale = scale_factor as f32;
                        let size = app.window.inner_size();
                        app.resize(size.width, size.height);
                    }
                    WindowEvent::Resized(size) => app.resize(size.width, size.height),
                    WindowEvent::RedrawRequested => {
                        app.render();
                        return;
                    }
                    WindowEvent::ModifiersChanged(mods) => app.mods = mods.state(),
                    WindowEvent::CursorMoved { position, .. } => {
                        let s = app.scale;
                        app.pointer_moved(position.x as f32 / s, position.y as f32 / s);
                    }
                    WindowEvent::CursorLeft { .. } => {
                        app.ui.set_hover(Hit::Nothing);
                        app.ui.set_link(None);
                    }
                    WindowEvent::MouseWheel { delta, phase, .. } => app.wheel(delta, phase),
                    WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
                        let (x, y) = app.mouse;
                        match button {
                            MouseButton::Left => app.click(x, y),
                            MouseButton::Middle => {
                                if let Hit::Tab(i) = app.ui.hit(x, y) {
                                    app.ui.close_tab(i);
                                }
                            }
                            MouseButton::Back => {
                                let load = app.ui.back();
                                app.run(load);
                            }
                            MouseButton::Forward => {
                                let load = app.ui.forward();
                                app.run(load);
                            }
                            _ => {}
                        }
                    }
                    WindowEvent::KeyboardInput { event, .. } => {
                        if event.state == ElementState::Pressed {
                            let typed = event.text.as_ref().map(|t| t.to_string());
                            if !app.key(&event.logical_key, typed.as_deref()) {
                                target.exit();
                            }
                        }
                    }
                    _ => return,
                }
                // Any input may start an animation; AboutToWait keeps it going.
                app.window.request_redraw();
            }
            _ => {}
        })
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Tests: the whole window, headless.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Loaded;

    fn fake_loaded(url: &str, title: &str) -> Loaded {
        let doc = crate::dom::parse_document(&format!("<h1>{title}</h1><p>Some text with <a href='/next'>a link</a>.</p>"));
        let fonts = Fonts::load();
        let sheet = crate::css::Stylesheet::default();
        let page = crate::layout::Layout::new(&fonts, &sheet, 1184.0).build(&doc);
        Loaded { url: Url::parse(url).unwrap(), title: title.into(), page, status: 200, rules: 0, note: "http 200".into() }
    }

    fn open(ui: &mut Ui, url: &str, title: &str) {
        let load = ui.navigate(Url::parse(url).unwrap());
        let fonts = Fonts::load();
        let loaded = fake_loaded(url, title);
        let thumb = thumbnail(&fonts, &loaded.page, 1184.0);
        ui.finish(load.tab_id, load.nav, Ok((loaded, thumb)));
    }

    fn settle(ui: &mut Ui) {
        for _ in 0..200 {
            ui.tick(0.016);
        }
    }

    fn frame(ui: &Ui, scale: f32) -> Canvas {
        let fonts = Fonts::load();
        let (w, h) = ((ui.w * scale) as usize, (ui.h * scale) as usize);
        let mut canvas = Canvas::new(w, h, theme::WINDOW);
        let view = ui.active_page().map(|(_, _, scroll, page)| {
            let v = ui.viewport();
            let mut c = Canvas::new((v.w * scale) as usize, (v.h * scale) as usize, theme::PAGE);
            c.paint_scaled(&fonts, page, scroll, scale);
            c
        });
        ui.draw(&mut canvas, &fonts, view.as_ref().map(|c| (c, 0)), scale);
        canvas
    }

    /// `SEPIA_UI_DUMP=/dir cargo test gui` writes the frames as PNGs for review.
    fn dump(canvas: &Canvas, name: &str) {
        if let Ok(dir) = std::env::var("SEPIA_UI_DUMP") {
            let _ = canvas.save_png(&std::path::Path::new(&dir).join(format!("{name}.png")));
        }
    }

    fn pixel(c: &Canvas, x: f32, y: f32, s: f32) -> [u8; 3] {
        let i = ((y * s) as usize * c.width + (x * s) as usize) * 4;
        [c.pixels[i], c.pixels[i + 1], c.pixels[i + 2]]
    }

    fn browsing() -> Ui {
        let mut ui = Ui::new(Transport::Tor, true);
        ui.set_size(1200.0, 800.0);
        open(&mut ui, "https://example.com/", "Example Domain");
        for (url, title) in [
            ("https://news.example/", "Новости дня"),
            ("https://docs.example/engine", "Документация движка"),
            ("https://maps.example/", "Карта города"),
        ] {
            ui.new_tab();
            open(&mut ui, url, title);
        }
        ui.select(0);
        settle(&mut ui);
        ui
    }

    #[test]
    fn transport_is_read_from_the_chain() {
        assert_eq!(Transport::of(&Chain::default(), None), Transport::Direct);
        assert_eq!(Transport::of(&Chain::parse("socks5://127.0.0.1:9050").unwrap(), None), Transport::Tor);
        assert_eq!(Transport::of(&Chain::parse("http://127.0.0.1:4444").unwrap(), None), Transport::I2p);
        assert_eq!(
            Transport::of(&Chain::parse("socks5://10.0.0.1:1080,socks5://10.0.0.2:1080").unwrap(), None),
            Transport::Chain(2)
        );
        let onion = Url::parse("http://abc.onion/").unwrap();
        assert_eq!(Transport::of(&Chain::default(), Some(&onion)), Transport::Tor);
    }

    #[test]
    fn start_screen_renders() {
        let mut ui = Ui::new(Transport::Direct, false);
        ui.set_size(1000.0, 700.0);
        settle(&mut ui);
        let c = frame(&ui, 1.0);
        dump(&c, "start");
        // the viewport card is the panel colour, the window around it darker
        assert_eq!(pixel(&c, 2.0, 300.0, 1.0), [theme::WINDOW.r, theme::WINDOW.g, theme::WINDOW.b]);
        assert_eq!(pixel(&c, 30.0, 300.0, 1.0), [theme::PANEL.r, theme::PANEL.g, theme::PANEL.b]);
    }

    #[test]
    fn history_back_forward_and_failures() {
        let mut ui = Ui::new(Transport::Tor, true);
        open(&mut ui, "https://a.example/", "A");
        open(&mut ui, "https://b.example/", "B");
        let back = ui.back().expect("can go back");
        assert_eq!(back.url.host, "a.example");
        ui.finish(back.tab_id, back.nav, Err("timed out".into()));
        assert!(ui.tab().failure.is_some());
        assert!(ui.active_page().is_none(), "failure screen, not the stale page");
        let fwd = ui.forward().expect("can go forward");
        assert_eq!(fwd.url.host, "b.example");
        // a superseded navigation is ignored
        let stale = ui.reload().unwrap();
        let fresh = ui.reload().unwrap();
        ui.finish(stale.tab_id, stale.nav, Err("stale".into()));
        assert!(ui.tab().loading.is_some());
        ui.finish(fresh.tab_id, fresh.nav, Err("fresh".into()));
        assert_eq!(ui.tab().failure.as_ref().unwrap().reason, "fresh");
    }

    #[test]
    fn address_input_rejects_non_addresses() {
        let mut ui = Ui::new(Transport::Tor, true);
        ui.begin_editing(false);
        ui.type_text("hello world");
        assert!(ui.submit().is_none());
        assert!(ui.toast.is_some());
        ui.begin_editing(false);
        ui.type_text("example.org/path");
        let load = ui.submit().expect("an address");
        assert_eq!(load.url.host, "example.org");
        assert!(!ui.editing);
    }

    #[test]
    fn hovered_card_faces_you_and_grows() {
        let mut ui = browsing();
        let g = ui.geom();
        let x = ui.slot_center(&g, 1);
        let hit = ui.hit(x, g.dock.y + 40.0);
        assert_eq!(hit, Hit::Tab(1));
        ui.set_hover(hit);
        settle(&mut ui);
        assert!(ui.tabs[1].anim.tilt.abs() < 0.5);
        assert!((ui.tabs[1].anim.scale - theme::MAGNIFY).abs() < 0.01);
        assert!(ui.tabs[0].anim.push < 0.0 && ui.tabs[2].anim.push > 0.0, "neighbours make room");
        // still hovered while the pointer is on the lifted, enlarged face
        let lifted = ui.hit(x, g.dock.y - 30.0);
        assert_eq!(lifted, Hit::Tab(1));
        let c = frame(&ui, 2.0);
        dump(&c, "dock-hover");
        // accent border of the hovered card shows above the dock
        // card bottom rests at dock.y + 12 + CARD_H, lifted; perspective makes it a bit taller
        let top = g.dock.y + 12.0 + theme::CARD_H - theme::LIFT - theme::CARD_H * theme::MAGNIFY * 1.12;
        let found = (0..40).any(|dy| {
            let px = pixel(&c, x, top + dy as f32, 2.0);
            // the mint accent edge over a white page: green clearly above red
            px[1] as i32 > px[0] as i32 + 25
        });
        assert!(found, "hovered card outline not found above the dock");
    }

    #[test]
    fn carousel_scrolls_and_keeps_new_tabs_visible() {
        let mut ui = Ui::new(Transport::Tor, true);
        ui.set_size(1200.0, 800.0);
        for _ in 0..20 {
            ui.new_tab();
        }
        let g = ui.geom();
        assert!(g.arrows);
        assert!(ui.active >= ui.offset && ui.active < ui.offset + g.visible, "new tab is in view");
        ui.page_by(-100);
        assert_eq!(ui.offset, 0);
        ui.carousel_wheel(80.0);
        assert_eq!(ui.offset, 2);
        settle(&mut ui);
        dump(&frame(&ui, 1.0), "carousel");
    }

    #[test]
    fn capsule_opens_on_hover() {
        let mut ui = browsing();
        let g = ui.geom();
        assert!(g.capsule.w <= theme::CAPSULE + 0.1);
        ui.set_hover(ui.hit(ui.w / 2.0, 28.0));
        settle(&mut ui);
        assert!(ui.geom().capsule.w > 400.0);
        dump(&frame(&ui, 2.0), "capsule-open");
    }

    #[test]
    fn gestures_commit_only_past_the_threshold() {
        let mut ui = browsing();
        open(&mut ui, "https://example.com/two", "Two");
        ui.gesture(60.0, 0.0);
        assert!(ui.gesture_end().is_none(), "short swipe does nothing");
        settle(&mut ui);
        for _ in 0..10 {
            ui.gesture(30.0, 2.0);
        }
        dump(&frame(&ui, 1.0), "swipe-back");
        let load = ui.gesture_end().expect("long swipe goes back");
        assert_eq!(load.url.path, "/");
        ui.finish(load.tab_id, load.nav, Err("x".into()));
        for _ in 0..10 {
            ui.gesture(0.0, 20.0);
        }
        assert!(ui.gesture_end().is_some(), "pull at the top reloads");
    }

    #[test]
    fn failure_screen_and_burn() {
        let mut ui = browsing();
        let load = ui.navigate(Url::parse("http://hidden.onion/").unwrap());
        ui.finish(load.tab_id, load.nav, Err("onion names live on Tor: start tor and use --via tor".into()));
        settle(&mut ui);
        dump(&frame(&ui, 1.0), "failure");
        ui.burn();
        assert_eq!(ui.tabs.len(), 1);
        assert!(ui.tab().url().is_none());
    }

    /// `cargo test --release frame_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn frame_cost() {
        let fonts = Fonts::load();
        let mut ui = browsing();
        ui.set_size(1440.0, 900.0);
        let g = ui.geom();
        ui.set_hover(Hit::Tab(1));
        for _ in 0..3 {
            ui.tick(0.016);
        }
        let s = 2.0;
        let v = ui.viewport();
        let (_, _, scroll, page) = ui.active_page().unwrap();
        let mut view = Canvas::new((v.w * s) as usize, (v.h * s) as usize, theme::PAGE);
        view.paint_scaled(&fonts, page, scroll, s);
        let mut canvas = Canvas::new((ui.w * s) as usize, (ui.h * s) as usize, theme::WINDOW);
        let time = |name: &str, f: &mut dyn FnMut()| {
            f();
            let start = Instant::now();
            for _ in 0..10 {
                f();
            }
            println!("{name:>12}: {:6.2} ms", start.elapsed().as_secs_f64() * 100.0);
        };
        time("whole frame", &mut || ui.draw(&mut canvas, &fonts, Some((&view, 0)), s));
        time("fill", &mut || Painter { c: &mut canvas, s }.fill(theme::WINDOW));
        time("viewport", &mut || ui.draw_viewport(&mut Painter { c: &mut canvas, s }, &fonts, &g, Some((&view, 0))));
        time("dock", &mut || ui.draw_dock(&mut Painter { c: &mut canvas, s }, &fonts, &g));
        time("cards", &mut || ui.draw_cards(&mut Painter { c: &mut canvas, s }, &fonts, &g));
        time("capsule", &mut || ui.draw_capsule(&mut Painter { c: &mut canvas, s }, &fonts, &g));
        let mut out = vec![0u32; canvas.width * canvas.height];
        time("to u32", &mut || {
            for (i, px) in canvas.pixels.chunks_exact(4).enumerate() {
                out[i] = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32;
            }
        });
        let mut start_ui = Ui::new(Transport::Tor, true);
        start_ui.set_size(1440.0, 900.0);
        time("start frame", &mut || start_ui.draw(&mut canvas, &fonts, None, s));

        // a long page (~5000 text runs, like a Wikipedia article), scrolled
        let html: String = (0..1200)
            .map(|i| format!("<p>Paragraph {i} with <b>bold</b> and <a href='/x{i}'>a link</a> and more words here.</p>"))
            .collect();
        let doc = crate::dom::parse_document(&html);
        let sheet = crate::css::Stylesheet::default();
        let big = crate::layout::Layout::new(&fonts, &sheet, v.w).build(&doc);
        let mut scroll_y = 20000.0;
        time("page repaint", &mut || {
            scroll_y += 37.0;
            view.paint_scaled(&fonts, &big, scroll_y, s)
        });
    }

    /// `SEPIA_BENCH_URL=https://… cargo test --release live_page_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_page_cost() {
        let Ok(address) = std::env::var("SEPIA_BENCH_URL") else { return };
        let fonts = Fonts::load();
        let url = Url::parse(&address).unwrap();
        let t = Instant::now();
        let loaded = page::load(&url, &Chain::default(), Some(&Resolver::default()), 1184.0, &fonts, Duration::from_secs(20), false).unwrap();
        println!("load+layout: {:7.1} ms, {} draws, height {:.0}", t.elapsed().as_secs_f64() * 1000.0, loaded.page.draws.len(), loaded.page.height);
        let s = 2.0;
        let mut view = Canvas::new((1184.0 * s) as usize, (780.0 * s) as usize, theme::PAGE);
        for scroll in [0.0f32, 37.0, 300.0] {
            let t = Instant::now();
            view.paint_scaled(&fonts, &loaded.page, scroll, s);
            println!("paint at scroll {scroll:5}: {:7.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
        let mut kinds: std::collections::BTreeMap<&str, (usize, f64)> = Default::default();
        for draw in &loaded.page.draws {
            let one = Page { draws: vec![draw.clone()], height: loaded.page.height, ..Page::default() };
            let t = Instant::now();
            view.paint_scaled(&fonts, &one, 0.0, s);
            let name = match draw {
                crate::layout::Draw::Rect { .. } => "rect",
                crate::layout::Draw::Border { .. } => "border",
                crate::layout::Draw::Text { .. } => "text",
                crate::layout::Draw::Image { .. } => "image",
                crate::layout::Draw::Fill { gradient: Some(_), .. } => "fill+gradient",
                crate::layout::Draw::Fill { .. } => "fill",
                crate::layout::Draw::Edges { .. } => "edges",
                crate::layout::Draw::Shadow { .. } => "shadow",
            };
            let e = kinds.entry(name).or_default();
            e.0 += 1;
            e.1 += t.elapsed().as_secs_f64() * 1000.0;
        }
        for (k, (n, ms)) in kinds {
            println!("  {k:14} x{n:4}: {ms:7.1} ms");
        }
        // the whole window as the user sees it
        let mut ui = Ui::new(Transport::Direct, true);
        ui.set_size(1200.0, 860.0);
        let load = ui.navigate(url.clone());
        let thumb = thumbnail(&fonts, &loaded.page, 1184.0);
        ui.finish(load.tab_id, load.nav, Ok((loaded, thumb)));
        settle(&mut ui);
        let c = frame(&ui, 2.0);
        dump(&c, "live");
    }

    #[test]
    fn page_frame_with_link_hint() {
        let mut ui = browsing();
        ui.set_link(Some("https://example.com/next".into()));
        let c = frame(&ui, 2.0);
        dump(&c, "page");
        // page area is white inside the viewport, window colour outside
        assert_eq!(pixel(&c, 600.0, 300.0, 2.0), [255, 255, 255]);
        assert_eq!(pixel(&c, 2.0, 300.0, 2.0)[0], theme::WINDOW.r, "window colour only in the inset margin");
        assert_eq!(pixel(&c, 100.0, 20.0, 2.0), [255, 255, 255], "the page runs to the top");
    }
}
