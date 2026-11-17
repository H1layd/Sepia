//! The window: Sepia's own chrome, drawn with our own rasteriser.
//!
//! Layout (logical px; multiplied by the display scale factor when drawn):
//!
//! * **viewport** — the page fills the window under the title bar.
//! * **island** (bottom centre) — three glass pieces: a pill of tab dots
//!   (the tab you are on is solid; clicking it opens all tabs), the capsule
//!   (the site's circle, title, address and the `tor · DE` chip; a click or
//!   ⌘L turns it into the address field with suggestions above it) and a
//!   round + button. While you read it tucks into a strip at the bottom edge.
//! * **⋯** (top right) — the menu: how sites see you, traffic, site passport,
//!   burn the session.
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
use std::path::PathBuf;
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
use crate::images::Bitmap;
use crate::js::{Outcome, PageEvent};
use crate::layout::{Draw, Fonts, Page, Rect};
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

    // Colourless glass: every colour of the chrome is a neutral grey; the
    // panels are frosted glass over the page with a bright rim on top.
    pub const WINDOW: Color = rgb(10, 10, 11);
    pub const PANEL: Color = rgb(17, 17, 19);
    pub const RAISED: Color = rgb(30, 30, 33);
    pub const STROKE: Color = rgb(40, 40, 44);
    pub const TEXT: Color = rgb(244, 244, 246);
    pub const TEXT_DIM: Color = rgb(150, 150, 156);
    #[allow(dead_code)]
    pub const ACCENT: Color = rgb(250, 250, 252);
    #[allow(dead_code)]
    pub const ON_ACCENT: Color = rgb(12, 12, 13);
    pub const DANGER: Color = rgb(250, 250, 252);
    pub const PAGE: Color = rgb(255, 255, 255);

    /// smoked glass: the tint laid over the blurred page
    pub const GLASS: Color = Color { r: 40, g: 40, b: 44, a: 118 };
    /// the same glass over a light page: a milky white wash
    pub const GLASS_LIGHT: Color = Color { r: 255, g: 255, b: 255, a: 120 };

    pub const TOR: Color = TEXT;
    pub const I2P: Color = TEXT;
    pub const CHAIN: Color = TEXT;
    pub const DIRECT: Color = TEXT_DIM;

    // geometry, logical px
    #[allow(dead_code)]
    pub const TOP: f32 = 56.0;
    pub const INSET: f32 = 0.0;
    pub const VIEW_RADIUS: f32 = 0.0;
    /// height of the (transparent) macOS title bar that lies over the page
    pub const TITLEBAR: f32 = 28.0;

    // the island: three glass pieces floating at the bottom centre —
    // the tab dots, the capsule (site + address), the + button
    pub const ISLAND_H: f32 = 52.0;
    pub const ISLAND_GAP: f32 = 10.0;
    pub const ISLAND_BOTTOM: f32 = 22.0;
    /// the capsule at rest; it shrinks to CAPSULE_MIN on narrow windows
    pub const CAPSULE_W: f32 = 440.0;
    pub const CAPSULE_MIN: f32 = 220.0;
    /// the capsule open as the address field
    pub const CAPSULE_OPEN_MAX: f32 = 640.0;
    pub const CAPSULE_OPEN_H: f32 = 56.0;
    /// the transport chip inside the capsule ("tor · DE")
    pub const CHIP: f32 = 96.0;
    /// a tab dot and the slot it stands in
    pub const DOT: f32 = 32.0;
    pub const SLOT: f32 = 38.0;
    /// room at each end of the dots, inside the pill
    pub const STRIP_PAD: f32 = 7.0;
    pub const ARROW: f32 = 22.0;
    pub const MAX_VISIBLE: usize = 9;

    // motion
    pub const PERSPECTIVE: f32 = 700.0;
    pub const EASE: f32 = 0.09; // seconds, time constant of every animation
    /// how long a tab opened in the background announces itself
    pub const PULSE: f32 = 1.6;
}

mod text {
    pub const NEW_TAB: &str = "Новая вкладка";
    pub const START_TITLE: &str = "Sepia";
    pub const START_HINT: &str = "Нажмите на капсулу внизу или ⌘L, чтобы ввести адрес";
    pub const GESTURES: &str =
        "Свайп двумя пальцами вправо — назад, влево — вперёд. Потяните страницу вниз — обновить.";
    pub const ERROR_TITLE: &str = "Страница не открылась";
    pub const ERROR_RETRY: &str = "Потяните страницу вниз или нажмите ⌘R, чтобы повторить";
    pub const LOADING: &str = "Загрузка";
    pub const BAD_ADDRESS: &str = "Это не похоже на адрес";
    pub const NOT_FOUND: &str = "На странице не найдено";
    pub const FIND_PLACEHOLDER: &str = "Найти на странице";
    pub const NO_BACK: &str = "Назад некуда";
    pub const NO_FORWARD: &str = "Вперёд некуда";
    pub const BURNED: &str = "Вкладки и история стёрты из памяти";
    pub const BOOKMARKS: &str = "Закладки";
    pub const BOOKMARK_ADDED: &str = "Добавлено в закладки";
    pub const BOOKMARK_REMOVED: &str = "Убрано из закладок";
    pub const BOOKMARK_HINT: &str = "⌘D — добавить страницу в закладки";
    pub const SAVED: &str = "Сохранено";
    pub const SETTINGS_TITLE: &str = "Как вас видят сайты";
    pub const SETTINGS_SUB: &str = "Браузер и страна, которыми Sepia представляется сайтам";
    pub const SETTINGS_BROWSER: &str = "Браузер";
    pub const SETTINGS_COUNTRY: &str = "Страна";
    pub const SETTINGS_FOOT: &str = "Действует сразу: открытая вкладка перезагрузится · Esc — закрыть";
    pub const SOON: &str = "скоро";
    pub const TRAFFIC_TITLE: &str = "Как идёт трафик";
    pub const TRAFFIC_FOOT: &str = "Способ задаётся при запуске: --via tor, --via i2p, --socks host:port · Esc — закрыть";
    pub const PASSPORT_TITLE: &str = "Паспорт сайта";
    pub const PASSPORT_EMPTY: &str = "Откройте страницу — здесь появится, что о ней известно";
    pub const PASSPORT_FOOT: &str = "Собрано из ответа сервера и кода страницы, без запросов к другим сервисам";
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

const THUMB_W: usize = 320;
const THUMB_H: usize = 208;

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
    /// the `#fragment` to scroll to once it is shown
    anchor: Option<String>,
    /// the history entry being left (its page is kept as a snapshot)
    from: usize,
}

/// A page left behind in a tab's history, kept in memory (with its script
/// session, which stays alive under `nav`): going back to it is instant.
struct Snapshot {
    index: usize,
    nav: u64,
    loaded: Loaded,
    thumb: Option<Image>,
    scroll: f32,
}

/// How many pages back (and forward) a tab keeps ready.
const SNAPSHOTS: usize = 6;

/// The system clipboard as text (macOS `pbpaste`; nothing elsewhere).
/// Local only: no network, no files.
fn clipboard_text() -> String {
    std::process::Command::new("pbpaste").output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn set_clipboard_text(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// `#fragment` out of a URL (our `Url` keeps it inside path or query).
fn split_fragment(url: Url) -> (Url, Option<String>) {
    let mut url = url;
    let mut fragment = None;
    for part in [&mut url.query, &mut url.path] {
        if let Some(at) = part.find('#') {
            fragment = Some(part[at + 1..].to_string());
            part.truncate(at);
            break;
        }
    }
    (url, fragment)
}

/// Find in page: matches on the active tab, drawn as highlights over a copy
/// of its page; `base` is the page without them.
struct FindState {
    tab_id: u64,
    /// the tab generation showing the highlights (anything newer drops them)
    generation: u64,
    base: Page,
    hits: Vec<Rect>,
    current: usize,
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
    snapshots: Vec<Snapshot>,
    anim: CardAnim,
    /// The site's icon and the host it belongs to: shown in place of the
    /// monogram only while the tab is still on that host.
    icon: Option<(String, Arc<Bitmap>)>,
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
            snapshots: Vec::new(),
            anim: CardAnim { scale: 1.0, ..CardAnim::default() },
            icon: None,
        }
    }

    /// The site's icon, if one came with a page of the host this tab is on.
    fn favicon(&self) -> Option<&Arc<Bitmap>> {
        let host = self.host();
        self.icon.as_ref().filter(|(h, _)| !host.is_empty() && *h == host).map(|(_, b)| b)
    }

    /// Remember `bitmap` as the icon of `host` (the backend hands it over
    /// with a loaded page; nothing here fetches it).
    fn set_icon(&mut self, host: &str, bitmap: Arc<Bitmap>) {
        self.icon = Some((host.trim_start_matches("www.").to_string(), bitmap));
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
    /// the × on the hovered tab card
    TabClose(usize),
    Transport,
    Burn,
    Dock,
    Page,
    /// a bookmark row on the start page
    Bookmark(usize),
    /// the settings sheet: its background, a browser row, a country row
    Settings,
    SettingsBrowser(usize),
    SettingsCountry(usize),
    /// the ⋯ button in the top right corner, a row of its menu
    Menu,
    MenuItem(usize),
    /// the background of an open panel (traffic, site passport)
    Panel,
    /// a row in the list above the open address field
    Suggest(usize),
    /// the all-tabs view: a card, its ×, the two buttons at the bottom
    OverviewTab(usize),
    OverviewClose(usize),
    OverviewNew,
    OverviewBurn,
}

/// A sheet opened from the ⋯ menu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Traffic,
    Passport,
}

/// The rows of the ⋯ menu, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MenuItem {
    Persona,
    Profiles,
    Traffic,
    Passport,
    Devtools,
    Plugins,
    Prefs,
    Burn,
}

pub const MENU: [MenuItem; 8] = [
    MenuItem::Persona,
    MenuItem::Profiles,
    MenuItem::Traffic,
    MenuItem::Passport,
    MenuItem::Devtools,
    MenuItem::Plugins,
    MenuItem::Prefs,
    MenuItem::Burn,
];

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            MenuItem::Persona => "Как меня видят сайты",
            MenuItem::Profiles => "Профили",
            MenuItem::Traffic => "Как идёт трафик",
            MenuItem::Passport => "Паспорт сайта",
            MenuItem::Devtools => "Инструменты разработчика",
            MenuItem::Plugins => "Плагины",
            MenuItem::Prefs => "Настройки браузера",
            MenuItem::Burn => "Сжечь сессию",
        }
    }

    /// Rows the engine cannot back yet are shown dimmed with «скоро».
    fn ready(self) -> bool {
        !matches!(self, MenuItem::Profiles | MenuItem::Devtools | MenuItem::Plugins | MenuItem::Prefs)
    }

    /// Rows are grouped; a thin line separates the groups.
    fn group(self) -> u8 {
        match self {
            MenuItem::Persona | MenuItem::Profiles | MenuItem::Traffic => 0,
            MenuItem::Burn => 2,
            _ => 1,
        }
    }
}

struct MenuLayout {
    button: Rect,
    panel: Rect,
    header: Rect,
    items: Vec<Rect>,
}

/// A page load the window should start on a worker thread.
#[derive(Debug, Clone)]
pub struct Load {
    pub tab_id: u64,
    pub nav: u64,
    pub url: Url,
    /// a form POST body (sent once; reloads and history go back to GET)
    pub form: Option<String>,
}

#[derive(Clone, Copy)]
struct Geom {
    viewport: Rect,
    /// the capsule: site and address at rest, the address field when open
    capsule: Rect,
    /// the whole island (all three pieces) — the dock
    dock: Rect,
    /// the round + button
    new_tab: Rect,
    /// the pill holding the tab dots, its arrows and its dot area
    pill: Rect,
    prev: Rect,
    strip: Rect,
    next: Rect,
    /// the transport chip inside the capsule
    chip: Rect,
    visible: usize,
    arrows: bool,
}

/// A row in the list above the open address field.
#[derive(Debug, Clone, PartialEq)]
pub enum Suggest {
    Tab(usize),
    Bookmark(usize),
    /// what Enter does with the typed text: open it or search for it
    Go,
}

struct SuggestRow {
    rect: Rect,
    kind: Suggest,
    title: String,
    aside: String,
    mark: char,
}

struct SuggestLayout {
    panel: Rect,
    headers: Vec<(f32, &'static str)>,
    rows: Vec<SuggestRow>,
}

struct OverviewLayout {
    /// a thumbnail per tab, in tab order
    cards: Vec<Rect>,
    header_y: f32,
    bar: Rect,
    new_tab: Rect,
    burn: Rect,
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
    /// the address field holds a find-in-page query, not an address
    pub finding: bool,
    find: Option<FindState>,
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
    /// 0 = the dock is tucked away as a strip at the bottom edge, 1 = shown
    dock_show: f32,
    /// seconds the dock stays up after the pointer leaves it
    dock_hold: f32,
    /// a tab card held by the mouse (dragging once it has moved)
    drag: Option<Drag>,
    /// a tab just opened in the background: (tab id, seconds left) — the
    /// dock comes up and its monogram sends out a ring so you notice it
    pulse: Option<(u64, f32)>,
    /// The settings sheet (who Sepia presents as): open or not, and its
    /// animation; the choice itself lives in `crate::persona`.
    pub settings: bool,
    settings_open: f32,
    /// the ⋯ menu in the top right corner, and the panel opened from it
    pub menu: bool,
    menu_open: f32,
    pub panel: Option<Panel>,
    /// the panel last shown, kept while it fades out
    panel_shown: Panel,
    panel_open: f32,
    /// the all-tabs view (click the active tab's dot, or ⌘⇧\)
    pub overview: bool,
    overview_open: f32,
    /// The only thing this browser ever writes to disk, and only when asked:
    /// (title, url) pairs stored as TSV in ~/.sepia/browser/bookmarks.tsv.
    bookmarks: Vec<(String, String)>,
}

struct SettingsSheet {
    panel: Rect,
    browsers: Vec<Rect>,
    countries: Vec<Rect>,
    divider: f32,
}

#[derive(Clone, Copy)]
struct Drag {
    index: usize,
    start_x: f32,
    /// pointer x minus the card centre when it was grabbed
    grab: f32,
    x: f32,
    moved: bool,
}

// ---------------------------------------------------------------------------
// Search and bookmarks
// ---------------------------------------------------------------------------

/// What the address field means when it is not an address: a search.
pub fn search_url(query: &str) -> Option<Url> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let encoded: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            b => format!("%{b:02X}"),
        })
        .collect();
    Url::parse(&format!("https://lite.duckduckgo.com/lite/?q={encoded}"))
}

fn bookmarks_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("SEPIA_BOOKMARKS") {
        return Some(std::path::PathBuf::from(path));
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".sepia").join("browser").join("bookmarks.tsv"))
}

/// One bookmark per line: `url<TAB>title`. Tabs and newlines inside fields are
/// folded away when saving, so the file always round-trips.
fn parse_bookmarks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (url, title) = match line.split_once('\t') {
            Some((u, t)) => (u, t),
            None => (line, ""),
        };
        // A real entry names a scheme explicitly; "not a url" must not pass.
        let plausible = url.contains("://") && !url.contains(char::is_whitespace);
        if plausible && Url::parse(url).is_some() {
            out.push((title.replace('\t', " "), url.to_string()));
        }
    }
    out
}

fn format_bookmarks(bookmarks: &[(String, String)]) -> String {
    let mut out = String::from("# sepia bookmarks: url<TAB>title\n");
    for (title, url) in bookmarks {
        let title = title.replace(['\t', '\n', '\r'], " ");
        out.push_str(url);
        out.push('\t');
        out.push_str(&title);
        out.push('\n');
    }
    out
}

fn bookmarks_load() -> Vec<(String, String)> {
    let Some(path) = bookmarks_path() else { return Vec::new() };
    std::fs::read_to_string(path).map(|text| parse_bookmarks(&text)).unwrap_or_default()
}

fn bookmarks_save(bookmarks: &[(String, String)]) {
    let Some(path) = bookmarks_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format_bookmarks(bookmarks));
}

fn downloads_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SEPIA_DOWNLOADS") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Downloads")
}

/// A safe file name for a URL: last path segment, or the host plus `.html`.
fn download_name(url: &Url) -> String {
    let last = url
        .path
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let mut stem: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '-' })
        .collect();
    if stem.is_empty() || stem.starts_with('.') {
        stem = if url.path.ends_with('/') || url.path.is_empty() {
            format!("{}.html", url.host)
        } else {
            "download".to_string()
        };
    }
    let stem = stem.trim_matches('-').to_string();
    let stem = if stem.is_empty() { "download".to_string() } else { stem };
    if stem.contains('.') {
        format!("sepia-{stem}")
    } else {
        format!("sepia-{stem}.html")
    }
}

/// `name`, `name-1`, `name-2`… in `dir`, so a download never overwrites a file.
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (name, String::new()),
    };
    for n in 1..1000 {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{:x}{ext}", std::process::id()))
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
            finding: false,
            find: None,
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
            dock_show: 1.0,
            dock_hold: 0.0,
            drag: None,
            pulse: None,
            settings: false,
            settings_open: 0.0,
            menu: false,
            menu_open: 0.0,
            panel: None,
            panel_shown: Panel::Traffic,
            panel_open: 0.0,
            overview: false,
            overview_open: 0.0,
            bookmarks: bookmarks_load(),
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
        // The page starts under the title bar, so the traffic lights never
        // cover it; the strip above is painted in the page's own top colour
        // (see `draw`), so it reads as part of the site, not as a bar.
        let viewport = Rect {
            x: theme::INSET,
            y: theme::TITLEBAR,
            w: w - theme::INSET * 2.0,
            h: h - theme::TITLEBAR - theme::INSET,
        };
        let gap = theme::ISLAND_GAP;
        let strip_w = |visible: usize| theme::STRIP_PAD * 2.0 + visible as f32 * theme::SLOT;
        let pill_w = |visible: usize, arrows: bool| strip_w(visible) + if arrows { theme::ARROW * 2.0 } else { 0.0 };
        let n = self.tabs.len().max(1);
        let mut visible = n.min(theme::MAX_VISIBLE);
        let mut arrows = n > visible;
        let mut cap_w = theme::CAPSULE_W;
        let room = w - 24.0;
        // too wide: the capsule gives way first, then the dots
        loop {
            let total = pill_w(visible, arrows) + gap + cap_w + gap + theme::ISLAND_H;
            if total <= room {
                break;
            }
            if cap_w > theme::CAPSULE_MIN {
                cap_w = (cap_w - (total - room)).max(theme::CAPSULE_MIN);
            } else if visible > 1 {
                visible -= 1;
                arrows = n > visible;
            } else {
                break;
            }
        }
        let total = pill_w(visible, arrows) + gap + cap_w + gap + theme::ISLAND_H;
        // tucked away, the island slides below the bottom edge
        let tuck = (theme::ISLAND_H + theme::ISLAND_BOTTOM + 24.0) * (1.0 - smooth(self.dock_show));
        let y = h - theme::ISLAND_BOTTOM - theme::ISLAND_H + tuck;
        let x0 = (w - total) / 2.0;
        let dock = Rect { x: x0, y, w: total, h: theme::ISLAND_H };
        let arrow_w = if arrows { theme::ARROW } else { 0.0 };
        let pill = Rect { x: x0, y, w: pill_w(visible, arrows), h: theme::ISLAND_H };
        let prev = Rect { x: x0, y, w: arrow_w, h: theme::ISLAND_H };
        let strip = Rect { x: x0 + arrow_w, y, w: strip_w(visible), h: theme::ISLAND_H };
        let next = Rect { x: strip.x + strip.w, y, w: arrow_w, h: theme::ISLAND_H };
        let rest = Rect { x: pill.x + pill.w + gap, y, w: cap_w, h: theme::ISLAND_H };
        let new_tab = Rect { x: rest.x + cap_w + gap, y, w: theme::ISLAND_H, h: theme::ISLAND_H };
        // open, the capsule grows into the address field across the middle
        let t = smooth(self.addr_open);
        let open_w = theme::CAPSULE_OPEN_MAX.min(w - 32.0);
        let open = Rect {
            x: (w - open_w) / 2.0,
            y: y + theme::ISLAND_H - theme::CAPSULE_OPEN_H,
            w: open_w,
            h: theme::CAPSULE_OPEN_H,
        };
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        let capsule = Rect { x: lerp(rest.x, open.x), y: lerp(rest.y, open.y), w: lerp(rest.w, open.w), h: lerp(rest.h, open.h) };
        let chip = Rect {
            x: capsule.x + capsule.w - 10.0 - theme::CHIP,
            y: capsule.y + (capsule.h - 30.0) / 2.0,
            w: theme::CHIP,
            h: 30.0,
        };
        Geom { viewport, capsule, dock, new_tab, pill, prev, strip, next, chip, visible, arrows }
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
        if self.settings {
            let sheet = self.settings_layout();
            for (i, r) in sheet.browsers.iter().enumerate() {
                if inside(r, 0.0) {
                    return Hit::SettingsBrowser(i);
                }
            }
            for (i, r) in sheet.countries.iter().enumerate() {
                if inside(r, 0.0) {
                    return Hit::SettingsCountry(i);
                }
            }
            // the sheet is modal: inside it nothing below reacts, outside closes it
            return if inside(&sheet.panel, 0.0) { Hit::Settings } else { Hit::Nothing };
        }
        if self.panel.is_some() {
            return if inside(&self.panel_rect(self.panel_shown), 0.0) { Hit::Panel } else { Hit::Nothing };
        }
        let menu = self.menu_layout();
        if inside(&menu.button, 4.0) {
            return Hit::Menu;
        }
        if self.menu {
            for (i, r) in menu.items.iter().enumerate() {
                if inside(r, 0.0) {
                    return Hit::MenuItem(i);
                }
            }
            // modal like the sheets: a click outside closes it
            return if inside(&menu.panel, 0.0) { Hit::Panel } else { Hit::Nothing };
        }
        if self.overview {
            let o = self.overview_layout();
            for (i, r) in o.cards.iter().enumerate() {
                let (bx, by) = overview_close(r);
                if (x - bx).powi(2) + (y - by).powi(2) <= 14.0f32.powi(2) {
                    return Hit::OverviewClose(i);
                }
                if inside(r, 0.0) || inside(&Rect { x: r.x, y: r.y + r.h, w: r.w, h: 44.0 }, 0.0) {
                    return Hit::OverviewTab(i);
                }
            }
            if inside(&o.new_tab, 0.0) {
                return Hit::OverviewNew;
            }
            if inside(&o.burn, 0.0) {
                return Hit::OverviewBurn;
            }
            return if inside(&o.bar, 0.0) { Hit::Panel } else { Hit::Nothing };
        }
        if self.dock_show < 0.5 && !self.editing {
            // only the strip at the bottom edge: touching it brings the island up
            let strip_zone = y >= self.h - 26.0 && x >= g.dock.x && x <= g.dock.x + g.dock.w;
            if strip_zone {
                return Hit::Dock;
            }
            return if inside(&g.viewport, 0.0) { Hit::Page } else { Hit::Nothing };
        }
        if self.addr_open > 0.5 {
            // the open address field and the list above it
            if self.editing && !self.finding {
                let s = self.suggest_layout();
                for (i, row) in s.rows.iter().enumerate() {
                    if inside(&row.rect, 0.0) {
                        return Hit::Suggest(i);
                    }
                }
                if inside(&s.panel, 0.0) {
                    return Hit::Capsule;
                }
            }
            if inside(&g.chip, 0.0) && !self.finding {
                return Hit::Transport;
            }
            if inside(&g.capsule, 6.0) {
                return Hit::Capsule;
            }
            return if inside(&g.viewport, 0.0) { Hit::Page } else { Hit::Nothing };
        }
        // the hovered dot keeps its × in its top-right corner
        if let Hit::Tab(i) | Hit::TabClose(i) = self.hover {
            if i < self.tabs.len() {
                let (bx, by) = close_badge(self.slot_center(&g, i), g.dock.y + g.dock.h / 2.0);
                if (x - bx).powi(2) + (y - by).powi(2) <= 9.0f32.powi(2) {
                    return Hit::TabClose(i);
                }
            }
        }
        if inside(&g.pill, 0.0) {
            if g.arrows && inside(&g.prev, 0.0) {
                return Hit::Prev;
            }
            if g.arrows && inside(&g.next, 0.0) {
                return Hit::Next;
            }
            let rel = (x - g.strip.x - theme::STRIP_PAD) / theme::SLOT + self.offset as f32;
            if rel >= 0.0 {
                let i = rel as usize;
                if i < self.tabs.len() && i >= self.offset && i < self.offset + g.visible {
                    return Hit::Tab(i);
                }
            }
            return Hit::Dock;
        }
        if inside(&g.chip, 0.0) {
            return Hit::Transport;
        }
        if inside(&g.capsule, 0.0) {
            return Hit::Capsule;
        }
        let (px, py) = (g.new_tab.x + g.new_tab.w / 2.0, g.new_tab.y + g.new_tab.h / 2.0);
        if (x - px).powi(2) + (y - py).powi(2) <= (g.new_tab.w / 2.0).powi(2) {
            return Hit::NewTab;
        }
        if inside(&g.dock, 0.0) {
            return Hit::Dock;
        }
        if inside(&g.viewport, 0.0) {
            let tab = self.tab();
            let on_start = tab.loaded.is_none() && tab.failure.is_none() && tab.loading.is_none();
            if on_start {
                for (i, row) in self.start_bookmark_rects(&g.viewport).iter().enumerate() {
                    if inside(row, 0.0) {
                        return Hit::Bookmark(i);
                    }
                }
            }
            return Hit::Page;
        }
        Hit::Nothing
    }

    /// Add or remove the page in the active tab; the only disk write we do.
    pub fn toggle_bookmark(&mut self) -> bool {
        let Some(url) = self.tab().url().cloned() else { return false };
        let url_text = url.to_string();
        if let Some(index) = self.bookmarks.iter().position(|(_, u)| *u == url_text) {
            self.bookmarks.remove(index);
            bookmarks_save(&self.bookmarks);
            self.toast(text::BOOKMARK_REMOVED);
            return false;
        }
        let title = self
            .active_loaded()
            .map(|l| {
                if l.title.is_empty() { url.host.clone() } else { l.title.clone() }
            })
            .unwrap_or_else(|| url.host.clone());
        self.bookmarks.insert(0, (title, url_text));
        bookmarks_save(&self.bookmarks);
        self.toast(text::BOOKMARK_ADDED);
        true
    }

    pub fn is_bookmarked(&self) -> bool {
        let Some(url) = self.tab().url() else { return false };
        let url_text = url.to_string();
        self.bookmarks.iter().any(|(_, u)| *u == url_text)
    }

    /// Where the start page's blocks begin; shared so hit-testing and drawing
    /// cannot drift apart.
    fn start_metrics(&self, v: &Rect) -> (f32, f32, f32) {
        let col = (v.w - 48.0).min(640.0);
        let left = v.x + (v.w - col) / 2.0;
        let mut y = v.y + (v.h * 0.22).max(56.0) + 34.0 + 36.0;
        let two_columns = col >= 520.0;
        let rows = if two_columns { 2.0 } else { 4.0 };
        y += rows * if two_columns { 76.0 } else { 70.0 } + 20.0;
        y += 2.0 * 19.0 + 20.0;
        (col, left, y)
    }

    /// Rows for the bookmarks list on the start page.
    fn start_bookmark_rects(&self, v: &Rect) -> Vec<Rect> {
        let (col, left, y) = self.start_metrics(v);
        let mut rows = Vec::new();
        if self.bookmarks.is_empty() {
            return rows;
        }
        let mut ry = y + 26.0;
        for _ in &self.bookmarks {
            rows.push(Rect { x: left, y: ry, w: col, h: 26.0 });
            ry += 28.0;
        }
        rows
    }

    pub fn open_settings(&mut self) {
        self.editing = false;
        self.settings = true;
    }

    pub fn close_settings(&mut self) {
        self.settings = false;
    }

    pub fn toggle_menu(&mut self) {
        self.menu = !self.menu;
        if self.menu {
            self.editing = false;
        }
    }

    pub fn close_menu(&mut self) {
        self.menu = false;
    }

    pub fn open_panel(&mut self, panel: Panel) {
        self.menu = false;
        self.editing = false;
        self.panel = Some(panel);
        self.panel_shown = panel;
    }

    pub fn close_panel(&mut self) {
        self.panel = None;
    }

    /// A row of the ⋯ menu was clicked. Burn is the caller's (it owns the
    /// sessions); everything else opens here. Returns the item.
    pub fn pick_menu(&mut self, index: usize) -> Option<MenuItem> {
        let item = *MENU.get(index)?;
        if !item.ready() {
            self.toast("Появится, когда движок это научится");
            return None;
        }
        match item {
            MenuItem::Persona => {
                self.menu = false;
                self.open_settings();
            }
            MenuItem::Traffic => self.open_panel(Panel::Traffic),
            MenuItem::Passport => self.open_panel(Panel::Passport),
            _ => self.menu = false,
        }
        Some(item)
    }

    /// The ⋯ button and its menu; shared by hit-testing and drawing.
    fn menu_layout(&self) -> MenuLayout {
        // below the title bar, so it never sits in the strip that drags the window
        let button = Rect { x: self.w - 14.0 - 30.0, y: theme::TITLEBAR + 10.0, w: 30.0, h: 30.0 };
        let w = 300.0f32.min(self.w - 24.0);
        let t = smooth(self.menu_open);
        let pad = 8.0;
        let x = self.w - 12.0 - w;
        let y = button.y + button.h + 8.0 - (1.0 - t) * 8.0;
        let header = Rect { x: x + pad, y: y + pad, w: w - pad * 2.0, h: 56.0 };
        let mut items = Vec::new();
        let mut ry = header.y + header.h + 9.0;
        let mut group = MENU[0].group();
        for item in MENU {
            if item.group() != group {
                group = item.group();
                ry += 9.0;
            }
            items.push(Rect { x: x + pad, y: ry, w: w - pad * 2.0, h: 36.0 });
            ry += 36.0;
        }
        let panel = Rect { x, y, w, h: ry + pad - y };
        MenuLayout { button, panel, header, items }
    }

    pub fn open_overview(&mut self) {
        self.editing = false;
        self.menu = false;
        self.overview = true;
    }

    pub fn close_overview(&mut self) {
        self.overview = false;
    }

    /// The all-tabs view: a grid of page thumbnails with a bar of two
    /// buttons at the bottom; shared by hit-testing and drawing.
    fn overview_layout(&self) -> OverviewLayout {
        let n = self.tabs.len().max(1);
        let pad = (self.w * 0.075).clamp(20.0, 96.0);
        let gap = 28.0;
        let header_y = theme::TITLEBAR + 46.0;
        let top = header_y + 30.0;
        let bottom = self.h - 24.0 - 52.0 - 24.0;
        let mut cols: usize = if self.w >= 1000.0 { 3 } else if self.w >= 620.0 { 2 } else { 1 };
        let label = 44.0;
        let fit_rows = |cols: usize, card_w: f32| {
            let rows = n.div_ceil(cols) as f32;
            rows * (card_w * 0.6 + label) + (rows - 1.0) * gap
        };
        let mut card_w = (self.w - pad * 2.0 - gap * (cols as f32 - 1.0)) / cols as f32;
        // many tabs: more columns, then smaller cards, until the grid fits
        while fit_rows(cols, card_w) > bottom - top && cols < 6 {
            cols += 1;
            card_w = (self.w - pad * 2.0 - gap * (cols as f32 - 1.0)) / cols as f32;
        }
        let rows = n.div_ceil(cols) as f32;
        let max_h = ((bottom - top - (rows - 1.0) * gap) / rows - label).max(40.0);
        let card_h = (card_w * 0.6).min(max_h);
        let card_w = card_h / 0.6;
        let grid_w = cols as f32 * card_w + (cols as f32 - 1.0) * gap;
        let left = (self.w - grid_w) / 2.0;
        let t = smooth(self.overview_open);
        let lift = (1.0 - t) * 18.0;
        let cards = (0..self.tabs.len())
            .map(|i| Rect {
                x: left + (i % cols) as f32 * (card_w + gap),
                y: top + lift + (i / cols) as f32 * (card_h + label + gap),
                w: card_w,
                h: card_h,
            })
            .collect();
        let bar = Rect { x: self.w / 2.0 - 129.0, y: self.h - 24.0 - 52.0, w: 258.0, h: 52.0 };
        let new_tab = Rect { x: bar.x + 8.0, y: bar.y + 8.0, w: 130.0, h: 36.0 };
        let burn = Rect { x: new_tab.x + new_tab.w + 6.0, y: bar.y + 8.0, w: 106.0, h: 36.0 };
        OverviewLayout { cards, header_y, bar, new_tab, burn }
    }

    /// What the list above the open address field offers for the typed text:
    /// matching open tabs, matching bookmarks, and what Enter will do.
    fn suggestions(&self) -> Vec<(Suggest, String, String, char)> {
        let q = self.input.trim().to_lowercase();
        let mut out = Vec::new();
        let matches = |a: &str, b: &str| q.is_empty() || a.to_lowercase().contains(&q) || b.to_lowercase().contains(&q);
        for (i, tab) in self.tabs.iter().enumerate() {
            if i == self.active || tab.url().is_none() {
                continue;
            }
            let (title, host) = (tab.title(), tab.host());
            if matches(&title, &host) && out.len() < 3 {
                out.push((Suggest::Tab(i), title, host, tab.monogram()));
            }
        }
        let tabs = out.len();
        for (i, (title, url)) in self.bookmarks.iter().enumerate() {
            if matches(title, url) && out.len() < tabs + 3 {
                let host = Url::parse(url).map(|u| u.host.trim_start_matches("www.").to_string()).unwrap_or_default();
                let mark = host.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_ascii_uppercase()).unwrap_or('•');
                out.push((Suggest::Bookmark(i), title.clone(), host, mark));
            }
        }
        if !q.is_empty() {
            let typed = self.input.trim();
            let address_like = !typed.contains(char::is_whitespace)
                && (typed.contains("://") || typed.contains('.') || typed.starts_with("localhost"));
            let aside = if address_like { "открыть" } else { "DuckDuckGo" };
            out.push((Suggest::Go, typed.to_string(), aside.to_string(), '↵'));
        }
        out
    }

    /// The list above the open address field: grouped rows in a glass panel.
    fn suggest_layout(&self) -> SuggestLayout {
        let g = self.geom();
        let c = g.capsule;
        let items = self.suggestions();
        let row_h = 40.0;
        let head_h = 26.0;
        let pad = 8.0;
        let mut headers = Vec::new();
        let mut rows = Vec::new();
        // measure top-down from 0, then shift above the capsule
        let mut y = pad;
        let mut last: Option<u8> = None;
        for (kind, title, aside, mark) in items {
            let group = match kind {
                Suggest::Tab(_) => 0,
                Suggest::Bookmark(_) => 1,
                Suggest::Go => 2,
            };
            if last != Some(group) {
                let head = match (group, &aside[..]) {
                    (0, _) => "Открытые вкладки",
                    (1, _) => "Закладки",
                    (_, "DuckDuckGo") => "Поиск · DuckDuckGo",
                    _ => "Адрес",
                };
                headers.push((y + 18.0, head));
                y += head_h;
                last = Some(group);
            }
            rows.push(SuggestRow { rect: Rect { x: c.x + pad, y, w: c.w - pad * 2.0, h: row_h - 2.0 }, kind, title, aside, mark });
            y += row_h;
        }
        let h = if rows.is_empty() { 0.0 } else { y + pad };
        let top = c.y - 10.0 - h;
        for (hy, _) in &mut headers {
            *hy += top;
        }
        for row in &mut rows {
            row.rect.y += top;
        }
        SuggestLayout { panel: Rect { x: c.x, y: top, w: c.w, h }, headers, rows }
    }

    /// A row of the list above the address field was clicked.
    pub fn pick_suggestion(&mut self, index: usize) -> Option<Load> {
        let (kind, ..) = self.suggestions().into_iter().nth(index)?;
        match kind {
            Suggest::Tab(i) => {
                self.editing = false;
                self.select(i);
                None
            }
            Suggest::Bookmark(i) => {
                let url = Url::parse(&self.bookmarks.get(i)?.1)?;
                self.editing = false;
                Some(self.navigate(url))
            }
            Suggest::Go => self.submit(),
        }
    }

    /// Where a panel sheet sits.
    fn panel_rect(&self, panel: Panel) -> Rect {
        let w = (self.w - 48.0).clamp(300.0, 640.0);
        let h = match panel {
            Panel::Traffic => 336.0,
            Panel::Passport => 372.0,
        };
        let t = smooth(self.panel_open);
        Rect {
            x: (self.w - w) / 2.0,
            y: ((self.h - h) / 2.0).max(theme::TITLEBAR + 8.0) + (1.0 - t) * 14.0,
            w,
            h,
        }
    }

    /// Where the settings sheet and its rows sit; shared by hit-testing and drawing.
    fn settings_layout(&self) -> SettingsSheet {
        let browsers = crate::persona::BROWSERS.len();
        let countries = crate::persona::COUNTRIES.len();
        let w = (self.w - 48.0).clamp(320.0, 760.0);
        let cols = if w >= 640.0 { 3 } else { 2 };
        let left_w = (w * 0.3).clamp(170.0, 230.0);
        let pad = 24.0;
        let row_h = 32.0;
        let country_rows = countries.div_ceil(cols);
        let rows = country_rows.max(browsers) as f32;
        let h = 72.0 + 26.0 + rows * row_h + 18.0 + 30.0;
        let t = smooth(self.settings_open);
        let panel = Rect { x: (self.w - w) / 2.0, y: ((self.h - h) / 2.0).max(theme::TITLEBAR + 8.0) + (1.0 - t) * 14.0, w, h };
        let top = panel.y + 72.0 + 26.0;
        let browsers = (0..browsers)
            .map(|i| Rect { x: panel.x + pad - 10.0, y: top + i as f32 * row_h, w: left_w, h: row_h - 2.0 })
            .collect();
        let gx = panel.x + pad + left_w + 16.0;
        let col_w = (panel.x + w - pad + 10.0 - gx) / cols as f32;
        let countries = (0..countries)
            .map(|i| Rect { x: gx + (i % cols) as f32 * col_w, y: top + (i / cols) as f32 * row_h, w: col_w - 4.0, h: row_h - 2.0 })
            .collect();
        SettingsSheet { panel, browsers, countries, divider: gx - 8.0 }
    }

    pub fn set_hover(&mut self, hit: Hit) {
        if self.drag.is_none() {
            self.hover = hit;
        }
    }

    // ----- dragging tabs ----------------------------------------------------

    /// The mouse went down on tab `index` at `x`: a click or the start of a drag.
    pub fn press_tab(&mut self, index: usize, x: f32) {
        if index >= self.tabs.len() {
            return;
        }
        let g = self.geom();
        let grab = x - self.slot_center(&g, index);
        self.drag = Some(Drag { index, start_x: x, grab, x, moved: false });
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The held card follows the pointer; crossing a neighbour's slot swaps
    /// the two, and the neighbour slides over into the freed slot.
    pub fn drag_to(&mut self, x: f32) {
        let Some(mut d) = self.drag else { return };
        d.x = x;
        if !d.moved && (x - d.start_x).abs() > 5.0 {
            d.moved = true;
        }
        if d.moved {
            let g = self.geom();
            let rel = (x - d.grab - g.strip.x - theme::STRIP_PAD) / theme::SLOT + self.offset_anim - 0.5;
            let first = self.offset;
            let last = (self.offset + g.visible).min(self.tabs.len()).saturating_sub(1);
            let target = (rel.round().max(0.0) as usize).clamp(first, last.max(first));
            while d.index != target {
                let next = if target > d.index { d.index + 1 } else { d.index - 1 };
                self.tabs.swap(d.index, next);
                // the neighbour now sits one slot over: keep it where it was
                // on screen and let it ease into place
                let shift = if next > d.index { theme::SLOT } else { -theme::SLOT };
                self.tabs[d.index].anim.push += shift;
                if self.active == d.index {
                    self.active = next;
                } else if self.active == next {
                    self.active = d.index;
                }
                d.index = next;
            }
        }
        self.drag = Some(d);
        self.hover = Hit::Tab(d.index);
    }

    /// The mouse came up. A press that never moved is a click: it selects
    /// the tab (returns true).
    pub fn release(&mut self) -> bool {
        let Some(d) = self.drag.take() else { return false };
        if !d.moved {
            // the dot of the tab you are on opens the all-tabs view
            if d.index == self.active {
                self.open_overview();
            } else {
                self.select(d.index);
            }
            return true;
        }
        false
    }

    // ----- navigation -----------------------------------------------------

    fn start_load(&mut self, index: usize, url: Url, push: bool) -> Load {
        let nav = self.next_nav;
        self.next_nav += 1;
        let (url, anchor) = split_fragment(url);
        let tab = &mut self.tabs[index];
        let from = tab.index;
        tab.loading = Some(Pending { nav, url: url.clone(), push, anchor, from });
        Load { tab_id: tab.id, nav, url, form: None }
    }

    /// Parse what was typed in the address field and load it in the active tab.
    pub fn submit(&mut self) -> Option<Load> {
        let typed = self.input.trim().to_string();
        // `yt something`: a YouTube search
        if let Some(query) = typed.strip_prefix("yt ").map(str::trim).filter(|q| !q.is_empty()) {
            let encoded: String = query
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
                    b' ' => "+".to_string(),
                    b => format!("%{b:02X}"),
                })
                .collect();
            if let Some(url) = Url::parse(&format!("https://www.youtube.com/results?search_query={encoded}")) {
                self.editing = false;
                return Some(self.start_load(self.active, url, true));
            }
        }
        let looks_like_address = !typed.is_empty()
            && !typed.contains(char::is_whitespace)
            && (typed.contains("://") || typed.contains('.') || typed.starts_with("localhost"));
        match Url::parse(&typed).filter(|_| looks_like_address) {
            Some(url) => {
                self.editing = false;
                Some(self.start_load(self.active, url, true))
            }
            None => {
                // not an address: search for it, like every other browser
                match search_url(&typed) {
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
        }
    }

    pub fn navigate(&mut self, url: Url) -> Load {
        self.start_load(self.active, url, true)
    }

    pub fn back(&mut self) -> Option<Load> {
        let tab = &self.tabs[self.active];
        if tab.index == 0 || tab.history.is_empty() {
            self.toast(text::NO_BACK);
            return None;
        }
        self.go_to(tab.index - 1)
    }

    pub fn forward(&mut self) -> Option<Load> {
        let tab = &self.tabs[self.active];
        if tab.index + 1 >= tab.history.len() {
            self.toast(text::NO_FORWARD);
            return None;
        }
        self.go_to(tab.index + 1)
    }

    /// Move the active tab to history entry `target`: from its snapshot when
    /// one is kept (instant, scroll and scripts as they were), else loaded.
    fn go_to(&mut self, target: usize) -> Option<Load> {
        let active = self.active;
        let tab = &mut self.tabs[active];
        if let Some(pos) = tab.snapshots.iter().position(|s| s.index == target) {
            let snap = tab.snapshots.remove(pos);
            if let Some(current) = tab.loaded.take() {
                tab.snapshots.push(Snapshot { index: tab.index, nav: tab.live_nav, loaded: current, thumb: tab.thumb.take(), scroll: tab.scroll });
            }
            tab.loaded = Some(snap.loaded);
            tab.thumb = snap.thumb;
            tab.scroll = snap.scroll;
            tab.live_nav = snap.nav;
            tab.index = target;
            tab.loading = None;
            tab.failure = None;
            tab.generation += 1;
            Self::trim_snapshots(tab);
            return None;
        }
        let url = tab.history[target].clone();
        let load = self.start_load(active, url, false);
        self.tabs[active].index = target;
        Some(load)
    }

    fn trim_snapshots(tab: &mut Tab) {
        // keep the entries nearest to where the tab is
        let at = tab.index;
        tab.snapshots.sort_by_key(|s| s.index.abs_diff(at));
        tab.snapshots.truncate(SNAPSHOTS);
    }

    /// Every (tab, nav) whose page is shown, loading or kept: their script
    /// sessions stay, the rest can end.
    pub fn kept_navs(&self) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        for tab in &self.tabs {
            out.push((tab.id, tab.live_nav));
            if let Some(p) = &tab.loading {
                out.push((tab.id, p.nav));
            }
            out.extend(tab.snapshots.iter().map(|s| (tab.id, s.nav)));
        }
        out
    }

    /// ⌘-click: the link opens in a new tab behind the current one.
    pub fn open_in_background(&mut self, url: Url) -> Load {
        let index = self.push_tab();
        self.pulse = Some((self.tabs[index].id, theme::PULSE));
        // bring the new card into the visible part of the strip
        let visible = self.geom().visible;
        if index >= self.offset + visible {
            self.offset = index + 1 - visible;
            self.clamp_offset();
        }
        self.start_load(index, url, true)
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
        // the page being left stays ready for going back to it
        if let Some(old) = tab.loaded.take() {
            if tab.failure.is_none() {
                tab.snapshots.retain(|s| s.index != pending.from);
                tab.snapshots.push(Snapshot { index: pending.from, nav: tab.live_nav, loaded: old, thumb: tab.thumb.take(), scroll: tab.scroll });
            }
        }
        if pending.push {
            if !tab.history.is_empty() {
                tab.history.truncate(tab.index + 1);
                // forward entries are gone, and so are their pages
                let keep = tab.index;
                tab.snapshots.retain(|s| s.index <= keep);
            }
            tab.history.push(pending.url.clone());
            tab.index = tab.history.len() - 1;
        }
        tab.generation += 1;
        tab.scroll = 0.0;
        match result {
            Ok((loaded, thumb)) => {
                if let Some(y) = pending.anchor.as_deref().and_then(|a| loaded.page.anchor(a)) {
                    tab.scroll = y.min(loaded.page.height).max(0.0);
                }
                if let Some(icon) = loaded.icon.clone() {
                    tab.set_icon(&loaded.url.host, icon);
                }
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
        Self::trim_snapshots(tab);
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
        self.drag = None;
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
        // every tab, with its kept pages and find highlights
        self.tabs.clear();
        self.find = None;
        self.finding = false;
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

    // ----- anchors and find in page ---------------------------------------

    /// A link to `#somewhere` in the page already shown: scroll, no load.
    /// True when it was handled that way.
    pub fn jump(&mut self, url: &Url) -> bool {
        let (bare, fragment) = split_fragment(url.clone());
        let Some(fragment) = fragment else { return false };
        let same = self.active_loaded().is_some_and(|l| split_fragment(l.url.clone()).0 == bare);
        same && self.jump_to(&fragment)
    }

    /// Scroll the active page to an anchor; false when there is none by that name.
    pub fn jump_to(&mut self, fragment: &str) -> bool {
        let Some(y) = self.active_loaded().and_then(|l| l.page.anchor(fragment)) else { return false };
        self.scroll_to(y);
        true
    }

    /// ⌘F: the address field becomes a search over the page.
    pub fn begin_find(&mut self) {
        if self.active_loaded().is_none() {
            return;
        }
        self.editing = true;
        self.finding = true;
        self.replace_input = !self.input.is_empty() && self.find.is_some();
        if self.find.is_none() {
            self.input.clear();
        }
    }

    /// Search again for what is typed and move `step` matches on (0 = stay
    /// on the nearest from the top of the screen, 1 = next, -1 = previous).
    pub fn find_step(&mut self, fonts: &Fonts, step: isize) {
        let query = self.input.trim().to_string();
        let active = self.active;
        let tab_id = self.tabs[active].id;
        let generation = self.tabs[active].generation;
        let previous = self.find.as_ref().and_then(|f| f.hits.get(f.current).copied());
        // the page without old highlights
        let state = self.find.take().filter(|f| f.tab_id == tab_id && f.generation == generation);
        let base = match state {
            Some(f) => f.base,
            None => match self.tabs[active].loaded.as_ref() {
                Some(l) => l.page.clone(),
                None => return,
            },
        };
        let hits = base.find(fonts, &query);
        let scroll = self.tabs[active].scroll;
        let current = if hits.is_empty() {
            0
        } else if step == 0 || previous.is_none() {
            hits.iter().position(|r| r.y >= scroll).unwrap_or(0)
        } else {
            let at = previous.and_then(|p| hits.iter().position(|r| r.y == p.y && r.x == p.x)).unwrap_or(0);
            (at as isize + step).rem_euclid(hits.len() as isize) as usize
        };
        let mut shown = base.clone();
        for (i, hit) in hits.iter().enumerate() {
            // colourless like the rest of the chrome: a mid-grey veil that
            // reads on light and dark pages alike, the current one denser
            let color = if i == current { Color { r: 112, g: 112, b: 120, a: 150 } } else { Color { r: 150, g: 150, b: 158, a: 80 } };
            shown.draws.push(Draw::Fill { rect: *hit, radius: 2.0, color: Some(color), gradient: None });
        }
        let tab = &mut self.tabs[active];
        if let Some(loaded) = tab.loaded.as_mut() {
            loaded.page = shown;
        }
        tab.generation += 1;
        let target = hits.get(current).map(|r| r.y);
        self.find = Some(FindState { tab_id, generation: tab.generation, base, hits, current });
        if let Some(y) = target {
            // bring the match into view, a third of the way down
            let v = self.viewport();
            let scroll = self.tabs[active].scroll;
            if y < scroll + 60.0 || y > scroll + v.h - 120.0 {
                self.scroll_to(y - v.h / 3.0);
            }
        } else if !query.is_empty() {
            self.toast(text::NOT_FOUND);
        }
    }

    /// Leave find: the field closes, highlights go away.
    pub fn end_find(&mut self) {
        self.finding = false;
        self.editing = false;
        if let Some(f) = self.find.take() {
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == f.tab_id && t.generation == f.generation) {
                if let Some(loaded) = tab.loaded.as_mut() {
                    loaded.page = f.base;
                    tab.generation += 1;
                }
            }
        }
    }

    /// (current match, 1-based; total) while finding.
    pub fn find_status(&self) -> Option<(usize, usize)> {
        let f = self.find.as_ref()?;
        Some((if f.hits.is_empty() { 0 } else { f.current + 1 }, f.hits.len()))
    }

    // ----- address field ----------------------------------------------------

    pub fn begin_editing(&mut self, prefill: bool) {
        if self.finding {
            self.end_find();
        }
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

    pub fn input_text(&self) -> &str {
        &self.input
    }

    /// ⌘A: the next key replaces the whole field.
    pub fn select_all(&mut self) {
        self.replace_input = !self.input.is_empty();
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
                let clearance = theme::ISLAND_H + theme::ISLAND_BOTTOM * 2.0;
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
            if self.find.as_ref().is_some_and(|f| f.tab_id == tab_id) {
                // the page changed under the highlights
                self.find = None;
            }
            let max = (loaded.page.height + theme::ISLAND_H + theme::ISLAND_BOTTOM * 2.0).max(0.0);
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
        let open = self.editing;
        moving |= ease(&mut self.overview_open, if self.overview { 1.0 } else { 0.0 }, dt * 0.7);
        moving |= ease(&mut self.addr_open, if open { 1.0 } else { 0.0 }, dt * 0.75);
        moving |= ease(&mut self.settings_open, if self.settings { 1.0 } else { 0.0 }, dt * 0.6);
        moving |= ease(&mut self.menu_open, if self.menu { 1.0 } else { 0.0 }, dt * 0.8);
        moving |= ease(&mut self.panel_open, if self.panel.is_some() { 1.0 } else { 0.0 }, dt * 0.6);
        moving |= ease(&mut self.offset_anim, self.offset as f32, dt);
        if !self.gesture_active {
            moving |= ease(&mut self.swipe, 0.0, dt);
            moving |= ease(&mut self.pull, 0.0, dt);
        }
        // the dock: up while the pointer is on it, a card is held, or there
        // is no page to look at; it tucks away a moment after the pointer leaves
        if let Some((_, left)) = &mut self.pulse {
            *left -= dt;
            moving = true;
            if *left <= 0.0 {
                self.pulse = None;
            }
        }
        let wants_dock = self.drag.is_some()
            || self.pulse.is_some()
            || self.active_loaded().is_none()
            || matches!(
                self.hover,
                Hit::Dock
                    | Hit::NewTab
                    | Hit::Prev
                    | Hit::Next
                    | Hit::Tab(_)
                    | Hit::TabClose(_)
                    | Hit::Transport
                    | Hit::Capsule
                    | Hit::Suggest(_)
            )
            || self.editing;
        if wants_dock {
            self.dock_hold = 0.7;
        } else if self.dock_hold > 0.0 {
            self.dock_hold -= dt;
            moving = true;
        }
        let up = if self.dock_hold > 0.0 { 1.0 } else { 0.0 };
        moving |= ease(&mut self.dock_show, up, dt * 0.8);

        let hovered = match (self.drag, self.hover) {
            (Some(d), _) => Some(d.index),
            (None, Hit::Tab(i) | Hit::TabClose(i)) => Some(i),
            _ => None,
        };
        let held = self.drag.filter(|d| d.moved).map(|d| {
            let g = self.geom();
            (d.index, d.x - d.grab - self.slot_center(&g, d.index))
        });
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let target = match held {
                // while a card is carried its neighbours stand at rest
                Some((h, _)) if h != i => card_target(i, None, i == active),
                _ => card_target(i, hovered, i == active),
            };
            let a = &mut tab.anim;
            match held {
                Some((h, push)) if h == i => {
                    a.push = push;
                    moving = true;
                }
                _ => moving |= ease(&mut a.push, target.push, dt),
            }
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

/// The colour along the top edge of the visible page: the average of a few
/// pixels near both side margins of its first row, where a site's
/// background shows rather than its text.
fn top_colour(view: &Canvas, row: usize) -> Color {
    if view.width < 8 || view.height == 0 {
        return theme::PAGE;
    }
    let y = row.min(view.height - 1);
    let mut sum = [0u32; 3];
    let xs = [1, 3, view.width - 2, view.width - 4];
    for x in xs {
        let i = (y * view.width + x) * 4;
        for c in 0..3 {
            sum[c] += view.pixels[i + c] as u32;
        }
    }
    let n = xs.len() as u32;
    rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8)
}

/// Centre of the × on a hovered card whose face has its top edge at `top`.
/// The × badge on a hovered tab dot centred at (cx, cy).
fn close_badge(cx: f32, cy: f32) -> (f32, f32) {
    (cx + theme::DOT / 2.0 - 3.0, cy - theme::DOT / 2.0 + 3.0)
}

/// The × in the top-right corner of a card in the all-tabs view.
fn overview_close(card: &Rect) -> (f32, f32) {
    (card.x + card.w - 24.0, card.y + 24.0)
}

/// Colours for things drawn on glass, adapted to what is behind it:
/// `t` = 0 over a dark backdrop (light ink), 1 over a light one (dark ink).
#[derive(Clone, Copy)]
struct Ink {
    t: f32,
}

impl Ink {
    fn text(&self) -> Color {
        mix(rgb(246, 246, 248), rgb(20, 20, 22), self.t)
    }
    fn dim(&self) -> Color {
        mix(rgb(168, 168, 174), rgb(92, 92, 98), self.t)
    }
    /// the text colour at opacity `a`: washes, hairlines, pills
    fn wash(&self, a: f32) -> Color {
        alpha(self.text(), a)
    }
    /// what goes on top of a solid `text()`-coloured shape
    fn inverse(&self) -> Color {
        mix(rgb(16, 16, 18), rgb(250, 250, 252), self.t)
    }
}

/// Where a tab dot eases to: the hovered one grows a little and lights up.
fn card_target(i: usize, hovered: Option<usize>, _active: bool) -> CardAnim {
    if hovered == Some(i) {
        CardAnim { scale: 1.12, hot: 1.0, ..CardAnim::default() }
    } else {
        CardAnim { scale: 1.0, ..CardAnim::default() }
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
    let run = |get: &dyn Fn(usize) -> [f32; 3], n: usize, out: &mut [[f32; 3]]| {
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
    /// over it: a frosted-glass panel. The blur runs on a copy downsampled by
    /// `f` (a wide blur loses nothing to it) and is read back bilinearly, so a
    /// dock-sized panel costs well under a millisecond. Three box-blur passes
    /// ≈ a Gaussian.
    fn frosted(&mut self, r: Rect, radius: f32, tint: Color, blur: f32, opacity: f32) {
        let s = self.s;
        let reach = (blur * s).ceil() as i32;
        let (cw, ch) = (self.c.width as i32, self.c.height as i32);
        let x0 = ((r.x * s).floor() as i32 - reach).max(0);
        let x1 = (((r.x + r.w) * s).ceil() as i32 + reach).min(cw);
        let y0 = ((r.y * s).floor() as i32 - reach).max(0);
        let y1 = (((r.y + r.h) * s).ceil() as i32 + reach).min(ch);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let f = ((blur * s) / 6.0).floor().clamp(1.0, 4.0) as usize;
        let (w, h) = (((x1 - x0) as usize).div_ceil(f), ((y1 - y0) as usize).div_ceil(f));
        let mut buf = vec![[0.0f32; 3]; w * h];
        let cwu = self.c.width;
        for by in 0..h {
            for bx in 0..w {
                let mut sum = [0.0f32; 3];
                let mut n = 0.0;
                for yy in (y0 as usize + by * f)..(y0 as usize + (by + 1) * f).min(y1 as usize) {
                    let row = yy * cwu;
                    for xx in (x0 as usize + bx * f)..(x0 as usize + (bx + 1) * f).min(x1 as usize) {
                        let i = (row + xx) * 4;
                        sum[0] += self.c.pixels[i] as f32;
                        sum[1] += self.c.pixels[i + 1] as f32;
                        sum[2] += self.c.pixels[i + 2] as f32;
                        n += 1.0;
                    }
                }
                buf[by * w + bx] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        }
        let pass = ((blur * s) / 3.0 / f as f32).round().max(1.0) as usize;
        for _ in 0..3 {
            box_blur(&mut buf, w, h, pass);
        }
        let shape = RoundShape::new(r, radius, s);
        let ta = tint.a as f32 / 255.0;
        let tc = [tint.r as f32 * ta, tint.g as f32 * ta, tint.b as f32 * ta];
        let keep = 1.0 - ta;
        let inv = 1.0 / f as f32;
        let (sx0, sx1, sy0, sy1) = shape.bounds(0.0);
        for y in sy0.max(y0)..sy1.min(y1) {
            let fy = ((y - y0) as f32 + 0.5) * inv - 0.5;
            let ya = (fy.floor().max(0.0) as usize).min(h - 1);
            let yb = (ya + 1).min(h - 1);
            let ty = (fy - ya as f32).clamp(0.0, 1.0);
            let (ia, ib) = shape.inner_span(y, 0.0).unwrap_or((sx1, sx1));
            for x in sx0.max(x0)..sx1.min(x1) {
                let cov = opacity * if x >= ia && x < ib { 1.0 } else { (0.5 - shape.sd(x, y)).clamp(0.0, 1.0) };
                if cov <= 0.0 {
                    continue;
                }
                let fx = ((x - x0) as f32 + 0.5) * inv - 0.5;
                let xa = (fx.floor().max(0.0) as usize).min(w - 1);
                let xb = (xa + 1).min(w - 1);
                let tx = (fx - xa as f32).clamp(0.0, 1.0);
                let (p00, p10, p01, p11) = (buf[ya * w + xa], buf[ya * w + xb], buf[yb * w + xa], buf[yb * w + xb]);
                let i = (y as usize * cwu + x as usize) * 4;
                for c in 0..3 {
                    let top = p00[c] + (p10[c] - p00[c]) * tx;
                    let bot = p01[c] + (p11[c] - p01[c]) * tx;
                    let glass = (top + (bot - top) * ty) * keep + tc[c];
                    let under = self.c.pixels[i + c] as f32;
                    self.c.pixels[i + c] = (glass * cov + under * (1.0 - cov)).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    /// A specular rim: a hairline that is bright along the top edge and fades
    /// towards the bottom, plus a faint inner glow line under the top edge —
    /// what makes a frosted panel read as a piece of glass.
    fn rim(&mut self, r: Rect, radius: f32, strength: f32) {
        let s = self.s;
        let shape = RoundShape::new(r, radius, s);
        let inner = RoundShape::new(Rect { x: r.x + 1.0, y: r.y + 1.0, w: r.w - 2.0, h: r.h - 2.0 }, (radius - 1.0).max(0.0), s);
        let half = 0.5 * s;
        let (top, height) = (r.y * s, (r.h * s).max(1.0));
        let (x0, x1, y0, y1) = shape.bounds(1.0);
        for y in y0..y1 {
            let v = ((y as f32 + 0.5 - top) / height).clamp(0.0, 1.0);
            let edge_a = strength * (0.10 + 0.42 * (1.0 - v).powi(3) + 0.10 * v.powi(4));
            let glow_a = strength * 0.16 * (1.0 - v * 3.0).max(0.0);
            let edge = Color { r: 255, g: 255, b: 255, a: (edge_a * 255.0).min(255.0) as u8 };
            let glow = Color { r: 255, g: 255, b: 255, a: (glow_a * 255.0).min(255.0) as u8 };
            let (ia, ib) = shape.inner_span(y, half * 2.0 + 2.0 * s + 1.0).unwrap_or((x1, x1));
            for x in (x0..ia.min(x1)).chain(ib.max(x0)..x1) {
                let d = shape.sd(x, y) + half;
                let cov = (half + 0.5 - d.abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.put(x, y, edge, cov);
                }
                if glow.a > 0 {
                    let d = inner.sd(x, y) + half;
                    let cov = (half + 0.5 - d.abs()).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        self.put(x, y, glow, cov);
                    }
                }
            }
        }
    }

    /// Mean brightness (0..1) of what is drawn under `r`, from a sparse grid.
    fn luma(&self, r: Rect) -> f32 {
        let s = self.s;
        let (cw, ch) = (self.c.width as f32, self.c.height as f32);
        let (mut sum, mut n) = (0.0, 0.0);
        for j in 0..5 {
            for i in 0..12 {
                let x = ((r.x + r.w * (i as f32 + 0.5) / 12.0) * s).clamp(0.0, cw - 1.0) as usize;
                let y = ((r.y + r.h * (j as f32 + 0.5) / 5.0) * s).clamp(0.0, ch - 1.0) as usize;
                let k = (y * self.c.width + x) * 4;
                let px = &self.c.pixels[k..k + 3];
                sum += 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
                n += 1.0;
            }
        }
        sum / n / 255.0
    }

    /// The ink for glass laid over `r`: light over dark pages, dark over light.
    fn ink_over(&self, r: Rect) -> Ink {
        Ink { t: smooth((self.luma(r) - 0.42) / 0.26) }
    }

    /// A floating glass panel that adapts to the site behind it: the page
    /// blurred under a smoky (dark sites) or milky (light sites) wash, a soft
    /// shadow and a specular rim. Returns the ink to draw on it with.
    fn glass(&mut self, r: Rect, radius: f32, blur: f32, shadow: f32, opacity: f32) -> Ink {
        let ink = self.ink_over(r);
        let tint = mix(theme::GLASS, theme::GLASS_LIGHT, ink.t);
        // fading glass stays glass: the panel's coverage simply follows `opacity`
        if opacity > 0.01 {
            self.frosted(r, radius, tint, blur, opacity.min(1.0));
        }
        if shadow > 0.0 {
            self.shadow(r, radius, 18.0, 8.0, shadow * opacity);
        }
        self.rim(r, radius, opacity * (1.0 - 0.5 * ink.t));
        if ink.t > 0.0 {
            // white rim vanishes on a white page: a faint dark hairline instead
            self.round_stroke(r, radius, 1.0, alpha(rgb(0, 0, 0), 0.09 * ink.t * opacity));
        }
        ink
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        self.round_rect(Rect { x: cx - r, y: cy - r, w: r * 2.0, h: r * 2.0 }, r, color);
    }

    /// A site icon centred on (cx, cy), `size` logical px, on a light plate of
    /// `plate` px with corner `radius` so dark logos stay readable on dark glass.
    fn favicon(&mut self, icon: &Bitmap, cx: f32, cy: f32, plate: f32, radius: f32, size: f32, opacity: f32) {
        if opacity <= 0.01 || icon.width == 0 || icon.height == 0 {
            return;
        }
        let pr = Rect { x: cx - plate / 2.0, y: cy - plate / 2.0, w: plate, h: plate };
        self.round_rect(pr, radius, alpha(rgb(250, 250, 252), opacity));
        // keep the aspect ratio inside a size×size box
        let k = (size / icon.width as f32).min(size / icon.height as f32);
        let (lw, lh) = (icon.width as f32 * k, icon.height as f32 * k);
        let s = self.s;
        let (dw, dh) = (((lw * s).round() as usize).max(1), ((lh * s).round() as usize).max(1));
        let px = icon.scaled(dw, dh);
        let left = ((cx - lw / 2.0) * s).round() as i32;
        let top = ((cy - lh / 2.0) * s).round() as i32;
        for y in 0..dh {
            let ty = top + y as i32;
            if ty < 0 || ty as usize >= self.c.height {
                continue;
            }
            for x in 0..dw {
                let tx = left + x as i32;
                if tx < 0 || tx as usize >= self.c.width {
                    continue;
                }
                let si = (y * dw + x) * 4;
                let a = px[si + 3] as f32 / 255.0 * opacity;
                if a <= 0.0 {
                    continue;
                }
                let di = (ty as usize * self.c.width + tx as usize) * 4;
                for ch in 0..3 {
                    // premultiplied source over
                    let v = px[si + ch] as f32 * opacity + self.c.pixels[di + ch] as f32 * (1.0 - a);
                    self.c.pixels[di + ch] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
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
            Icon::Alert => {
                self.polyline(&map(&[(12.0, 6.5), (12.0, 13.5)]), w * 1.3, color);
                self.polyline(&map(&[(12.0, 17.5)]), w * 1.6, color);
            }
            Icon::Search => {
                let (lx, ly) = (cx + (10.5 - 12.0) * k, cy + (10.5 - 12.0) * k);
                self.arc(lx, ly, 6.0 * k, 0.0, std::f32::consts::TAU, w, color);
                self.polyline(&map(&[(15.0, 15.0), (19.5, 19.5)]), w * 1.1, color);
            }
            Icon::Close => {
                self.polyline(&map(&[(7.0, 7.0), (17.0, 17.0)]), w * 1.2, color);
                self.polyline(&map(&[(17.0, 7.0), (7.0, 17.0)]), w * 1.2, color);
            }
            Icon::Reload => {
                self.arc(cx, cy, 7.0 * k, -0.6, 5.2, w, color);
                let (ex, ey) = (cx + 7.0 * k * (-0.6f32).cos(), cy + 7.0 * k * (-0.6f32).sin());
                self.polyline(&[(ex - 4.0 * k, ey - 1.0 * k), (ex, ey), (ex + 0.6 * k, ey - 4.2 * k)], w, color);
            }
            Icon::Bookmark => self.polyline(
                &map(&[
                    (7.0, 4.0), (17.0, 4.0), (17.0, 20.0), (12.0, 15.5), (7.0, 20.0), (7.0, 4.0),
                ]),
                w,
                color,
            ),
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
    Alert,
    Reload,
    Close,
    Search,
    Bookmark,
}

/// What a tab card shows on its face.
enum Face<'a> {
    Thumb(&'a Image),
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
        let band = Rect { x: 0.0, y: 0.0, w: self.w, h: g.viewport.y };
        let band_color = match (page_view, &self.tab().failure) {
            (Some((view, row)), None) => top_colour(view, row),
            _ => theme::PANEL,
        };
        p.round_rect(band, 0.0, band_color);
        self.draw_viewport(&mut p, fonts, &g, page_view);
        self.draw_link(&mut p, fonts, &g);
        self.draw_dock(&mut p, fonts, &g);
        self.draw_capsule(&mut p, fonts, &g);
        self.draw_gestures(&mut p, &g);
        self.draw_overview(&mut p, fonts, &g);
        self.draw_menu(&mut p, fonts);
        self.draw_panel(&mut p, fonts);
        self.draw_settings(&mut p, fonts);
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
                // dark on light sites, light on dark ones
                let bar = Rect { x: x0, y: v.y, w: x1 - x0, h: 2.5 };
                let ink = p.ink_over(Rect { y: v.y, h: 6.0, ..bar });
                p.round_rect(bar, 1.25, ink.wash(0.75));
            }
        }
    }

    fn draw_start(&self, p: &mut Painter, fonts: &Fonts, v: &Rect) {
        let (col, left, _) = self.start_metrics(v);
        let mut y = v.y + (v.h * 0.22).max(56.0);
        p.text_centered(fonts, text::START_TITLE, v.x + v.w / 2.0, y, 34.0, theme::TEXT, true);
        y += 34.0;
        p.text_centered(fonts, &fit(fonts, text::START_HINT, 14.0, false, col), v.x + v.w / 2.0, y, 14.0, theme::TEXT_DIM, false);
        y += 36.0;

        let route = self.transport.describe();
        let facts: [(&str, String, Color); 4] = [
            ("Маршрут", route, self.transport.color()),
            ("DNS", if self.doh { "через DoH, не системным резолвером".into() } else { "системный резолвер".into() }, if self.doh { theme::TEXT } else { theme::DIRECT }),
            ("JavaScript", "в песочнице: без сети, файлов и WebRTC".into(), theme::TEXT),
            ("Куки и диск", "куки в памяти, на диск — только закладки".into(), theme::TEXT),
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
            p.round_rect(r, 16.0, rgb(22, 22, 25));
            p.round_stroke(r, 16.0, 1.0, theme::STROKE);
            p.text(fonts, label, r.x + 16.0, r.y + 24.0, 12.0, theme::TEXT_DIM, false);
            p.text(fonts, &fit(fonts, value, 14.0, false, card_w - 32.0), r.x + 16.0, r.y + 46.0, 14.0, *color, false);
        }
        let rows = if two_columns { 2.0 } else { 4.0 };
        y += rows * if two_columns { 76.0 } else { 70.0 } + 20.0;
        for line in wrap(fonts, text::GESTURES, 13.0, col, 2) {
            p.text_centered(fonts, &line, v.x + v.w / 2.0, y, 13.0, theme::TEXT_DIM, false);
            y += 19.0;
        }

        if self.bookmarks.is_empty() {
            p.text_centered(fonts, text::BOOKMARK_HINT, v.x + v.w / 2.0, y + 18.0, 12.0, theme::TEXT_DIM, false);
            return;
        }
        p.text(fonts, text::BOOKMARKS, left, y, 12.0, theme::TEXT_DIM, false);
        for (row, (title, url)) in self.start_bookmark_rects(v).iter().zip(self.bookmarks.iter()) {
            p.round_rect(*row, 10.0, rgb(22, 22, 25));
            p.round_stroke(*row, 10.0, 1.0, theme::STROKE);
            let label = fit(fonts, title, 13.0, false, row.w * 0.6);
            p.text(fonts, &label, row.x + 12.0, row.y + 18.0, 13.0, theme::TEXT, false);
            let host = fit(fonts, url, 11.0, false, row.w * 0.36);
            let host_w = fonts.advance(&host, 11.0, false, false);
            p.text(fonts, &host, row.x + row.w - 12.0 - host_w, row.y + 18.0, 11.0, theme::TEXT_DIM, false);
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
        let bottom = g.dock.y.min(self.h);
        let (max_w, y) = if room >= 160.0 {
            (room.min(440.0), g.viewport.y + g.viewport.h - 36.0)
        } else {
            ((g.viewport.w - 24.0).min(440.0), bottom - 36.0)
        };
        let label = fit(fonts, href, 12.0, false, max_w - 20.0);
        let w = fonts.advance(&label, 12.0, false, false) + 20.0;
        let r = Rect { x: g.viewport.x + 12.0, y, w, h: 24.0 };
        let ink = p.glass(r, 12.0, 10.0, 0.0, 1.0);
        p.text(fonts, &label, r.x + 10.0, r.y + 16.0, 12.0, ink.text(), false);
    }

    fn draw_dock(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let shown = smooth(self.dock_show);
        let a = (1.0 - shown * 2.5).max(0.0);
        if a > 0.0 && !self.editing {
            // tucked away: a slim strip along the bottom edge; it is gone
            // before the rising island reaches it, so it never shows through
            let r = Rect { x: self.w / 2.0 - 60.0, y: self.h - 10.0 + 6.0 * shown, w: 120.0, h: 5.0 };
            let ink = p.ink_over(Rect { x: r.x, y: r.y - 6.0, w: r.w, h: 16.0 });
            p.shadow(r, 2.5, 6.0, 1.0, 0.18 * a * (1.0 - ink.t));
            p.round_rect(r, 2.5, ink.wash(0.42 * a));
        }
        if shown < 0.01 {
            return;
        }
        // the dots and the + step aside while the capsule is the address field
        let side = shown * (1.0 - smooth(self.addr_open));
        if side < 0.01 {
            return;
        }
        let h = theme::ISLAND_H;
        let ink = p.glass(g.pill, h / 2.0, 22.0, 0.16, side);
        if g.arrows {
            let can_prev = self.offset > 0;
            let can_next = self.offset < self.max_offset();
            let tone = |on: bool, hit: Hit| {
                alpha(
                    if !on {
                        alpha(ink.dim(), 0.35)
                    } else if self.hover == hit {
                        ink.text()
                    } else {
                        ink.dim()
                    },
                    side,
                )
            };
            let my = g.dock.y + h / 2.0;
            p.icon(Icon::Left, g.prev.x + g.prev.w / 2.0 + 3.0, my, 14.0, tone(can_prev, Hit::Prev));
            p.icon(Icon::Right, g.next.x + g.next.w / 2.0 - 3.0, my, 14.0, tone(can_next, Hit::Next));
        }
        self.draw_cards(p, fonts, g, ink);

        // the round + button
        let b = g.new_tab;
        let ink = p.glass(b, b.h / 2.0, 22.0, 0.16, side);
        if self.hover == Hit::NewTab {
            p.circle(b.x + b.w / 2.0, b.y + b.h / 2.0, b.w / 2.0 - 5.0, ink.wash(0.10 * side));
        }
        p.icon(Icon::Plus, b.x + b.w / 2.0, b.y + b.h / 2.0, 19.0, alpha(ink.text(), side));
    }

    /// The tab dots: a circle per tab with its icon or monogram; the tab you
    /// are on is the solid one. The hovered dot grows, shows a × and a plate
    /// with the title above.
    fn draw_cards(&self, p: &mut Painter, fonts: &Fonts, g: &Geom, ink: Ink) {
        let side = smooth(self.dock_show) * (1.0 - smooth(self.addr_open));
        let clip = (g.strip.x + 2.0, g.strip.x + g.strip.w - 2.0);
        let cy = g.dock.y + g.dock.h / 2.0;
        let mut label: Option<(f32, usize)> = None;
        for (i, tab) in self.tabs.iter().enumerate() {
            let a = tab.anim;
            let carried = self.drag.is_some_and(|d| d.moved && d.index == i);
            let cx = self.slot_center(g, i) + a.push;
            if !carried && (cx < clip.0 || cx > clip.1) {
                continue;
            }
            let active = i == self.active;
            let r = theme::DOT / 2.0 * a.scale;
            if carried {
                p.shadow(Rect { x: cx - r, y: cy - r, w: r * 2.0, h: r * 2.0 }, r, 10.0, 6.0, 0.35 * side);
            }
            if let Some(icon) = tab.favicon() {
                p.favicon(icon, cx, cy, r * 2.0, r, r * 1.15, side);
                if active {
                    let ring = Rect { x: cx - r - 2.5, y: cy - r - 2.5, w: r * 2.0 + 5.0, h: r * 2.0 + 5.0 };
                    p.round_stroke(ring, ring.w / 2.0, 1.8, alpha(ink.text(), side));
                }
            } else {
                let bg = if active { ink.text() } else { mix(ink.wash(0.10), ink.wash(0.22), a.hot) };
                p.circle(cx, cy, r, alpha(bg, side));
                let fg = if active { ink.inverse() } else { mix(ink.dim(), ink.text(), a.hot) };
                let letter = tab.monogram().to_string();
                p.text_centered(fonts, &letter, cx, cy + r * 0.27, r * 0.78, alpha(fg, side), true);
            }
            if tab.loading.is_some() {
                p.arc(cx, cy, r + 3.0, self.spin * 6.0, 1.6, 1.6, alpha(ink.text(), side));
            }
            if let Some((_, left)) = self.pulse.filter(|(id, _)| *id == tab.id) {
                // two rings expand from the dot and fade
                let phase = 1.0 - left / theme::PULSE;
                for k in 0..2 {
                    let f = (phase * 2.0 - k as f32 * 0.5).rem_euclid(1.0);
                    let rr = r + 2.0 + 9.0 * f;
                    let ring = Rect { x: cx - rr, y: cy - rr, w: rr * 2.0, h: rr * 2.0 };
                    p.round_stroke(ring, rr, 1.5, ink.wash(0.7 * (1.0 - f) * side));
                }
            }
            if a.hot > 0.05 {
                label = Some((cx, i));
            }
        }
        let Some((cx, i)) = label else { return };
        let tab = &self.tabs[i];
        let t = tab.anim.hot * side;
        // the × badge (not while carried)
        if self.drag.is_none_or(|d| !d.moved) && t > 0.3 {
            let (bx, by) = close_badge(cx, cy);
            let hot = self.hover == Hit::TabClose(i);
            let k = ((t - 0.3) / 0.7).min(1.0);
            p.shadow(Rect { x: bx - 8.0, y: by - 8.0, w: 16.0, h: 16.0 }, 8.0, 5.0, 1.5, 0.3 * k);
            p.circle(bx, by, 8.0, alpha(if hot { rgb(250, 250, 252) } else { rgb(36, 36, 40) }, 0.95 * k));
            p.icon(Icon::Close, bx, by, 10.0, alpha(if hot { rgb(16, 16, 18) } else { rgb(236, 236, 240) }, k));
        }
        if self.drag.is_some_and(|d| d.moved) {
            return;
        }
        // title plate above the island
        let title = fit(fonts, &tab.title(), 13.0, true, 300.0);
        let host = tab.host();
        let tw = fonts.advance(&title, 13.0, true, false).max(fonts.advance(&host, 11.0, false, false));
        let h = if host.is_empty() { 30.0 } else { 44.0 };
        let w = tw + 24.0;
        let x = (cx - w / 2.0).clamp(8.0, self.w - w - 8.0);
        let r = Rect { x, y: g.dock.y - 10.0 - h + (1.0 - t) * 8.0, w, h };
        let pink = p.glass(r, 14.0, 14.0, 0.16, t);
        p.text_centered(fonts, &title, r.x + w / 2.0, r.y + 20.0, 13.0, alpha(pink.text(), t), true);
        if !host.is_empty() {
            p.text_centered(fonts, &host, r.x + w / 2.0, r.y + 35.0, 11.0, alpha(pink.dim(), t), false);
        }
    }

    /// The capsule: the site's circle, its title and address, the transport
    /// chip. Open, it is the address field (or the find field), with the
    /// list of suggestions above it.
    fn draw_capsule(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let shown = smooth(self.dock_show).max(smooth(self.addr_open));
        if shown < 0.01 {
            return;
        }
        let c = g.capsule;
        let tab = self.tab();
        let open = ((self.addr_open - 0.35) / 0.65).clamp(0.0, 1.0);
        let ink = p.glass(c, c.h / 2.0, 22.0, 0.16 + 0.08 * open, shown);
        if open > 0.0 {
            p.round_stroke(c, c.h / 2.0, 1.0, ink.wash(0.10 * open));
        }
        let my = c.y + c.h / 2.0;
        let fade = |c: Color, k: f32| alpha(c, k * shown);
        let transport = Transport::of_tab(self.transport, tab);
        let finding = self.finding && self.editing;

        // the transport chip on the right (not while finding)
        let chip_k = if finding { 1.0 - open } else { 1.0 };
        if chip_k > 0.01 {
            let r = g.chip;
            let hot = self.hover == Hit::Transport;
            p.round_rect(r, r.h / 2.0, ink.wash((if hot { 0.16 } else { 0.08 }) * shown * chip_k));
            let dot = if transport == Transport::Direct { ink.dim() } else { ink.text() };
            p.circle(r.x + 14.0, my, 3.0, fade(dot, chip_k));
            let label = format!("{} · {}", transport.label(), crate::persona::current().country.code);
            let label = fit(fonts, &label, 12.0, false, r.w - 30.0);
            p.text(fonts, &label, r.x + 24.0, my + 4.0, 12.0, fade(ink.text(), chip_k), false);
        }

        let rest = 1.0 - open;
        if rest > 0.01 {
            // at rest: the site's circle, its title, its address
            let (cx, r) = (c.x + 9.0 + 17.0, 17.0);
            if let Some(icon) = tab.favicon() {
                p.favicon(icon, cx, my, r * 2.0, r, 20.0, rest * shown);
            } else {
                p.circle(cx, my, r, fade(rgb(250, 250, 250), rest));
                p.text_centered(fonts, &tab.monogram().to_string(), cx, my + 5.0, 14.0, fade(rgb(17, 17, 17), rest), true);
            }
            if tab.loading.is_some() {
                p.arc(cx, my, r + 3.0, self.spin * 6.0, 1.8, 1.8, fade(ink.text(), rest));
            }
            let tx = cx + r + 12.0;
            let room = g.chip.x - 10.0 - tx;
            let (title, sub) = match tab.url() {
                Some(_) => (tab.title(), tab.host()),
                None => (text::NEW_TAB.to_string(), "Нажмите, чтобы ввести адрес".to_string()),
            };
            let mut title_room = room;
            if self.is_bookmarked() {
                p.icon(Icon::Bookmark, tx + room - 7.0, my, 13.0, fade(ink.dim(), rest));
                title_room -= 18.0;
            }
            p.text(fonts, &fit(fonts, &title, 14.0, true, title_room), tx, my - 2.0, 14.0, fade(ink.text(), rest), true);
            p.text(fonts, &fit(fonts, &sub, 12.0, false, room), tx, my + 14.0, 12.0, fade(ink.dim(), rest), false);
        }
        if open <= 0.0 {
            return;
        }
        let sx = c.x + 24.0;
        p.icon(Icon::Search, sx, my, 17.0, fade(ink.text(), open));
        if finding {
            self.draw_find_field(p, fonts, c, my, ink, open);
            return;
        }
        let text_x = c.x + 46.0;
        let esc_w = fonts.advance("esc", 12.0, false, false);
        let esc_x = g.chip.x - 12.0 - esc_w;
        p.text(fonts, "esc", esc_x, my + 4.0, 12.0, fade(ink.dim(), open), false);
        let room = esc_x - 12.0 - text_x;
        if room > 24.0 {
            let shown_text = fit_tail(fonts, &self.input, 17.0, room - 4.0);
            if self.replace_input && !shown_text.is_empty() {
                // everything selected: a soft highlight behind the text
                let sw = fonts.advance(&shown_text, 17.0, false, false);
                p.round_rect(Rect { x: text_x - 3.0, y: my - 12.0, w: sw + 6.0, h: 24.0 }, 6.0, ink.wash(0.16 * open));
            }
            p.text(fonts, &shown_text, text_x, my + 6.0, 17.0, fade(ink.text(), open), false);
            let caret_x = text_x + fonts.advance(&shown_text, 17.0, false, false) + 1.0;
            p.round_rect(Rect { x: caret_x, y: my - 10.0, w: 1.5, h: 20.0 }, 0.75, fade(ink.text(), open));
            if self.input.is_empty() {
                p.text(fonts, "Адрес или поиск", text_x + 6.0, my + 6.0, 17.0, fade(ink.dim(), open), false);
            }
        }
        self.draw_suggestions(p, fonts, open);
    }

    /// The glass list above the open address field.
    fn draw_suggestions(&self, p: &mut Painter, fonts: &Fonts, t: f32) {
        let s = self.suggest_layout();
        if s.rows.is_empty() {
            return;
        }
        let r = Rect { y: s.panel.y + (1.0 - t) * 10.0, ..s.panel };
        let dy = r.y - s.panel.y;
        let ink = p.glass(r, 24.0, 24.0, 0.18, t);
        let fade = |c: Color| alpha(c, t);
        for (y, head) in &s.headers {
            p.text(fonts, head, r.x + 20.0, y + dy, 11.0, fade(ink.dim()), true);
        }
        for (i, row) in s.rows.iter().enumerate() {
            let rect = Rect { y: row.rect.y + dy, ..row.rect };
            let go = row.kind == Suggest::Go;
            if self.hover == Hit::Suggest(i) {
                p.round_rect(rect, 14.0, ink.wash(0.10 * t));
            } else if go {
                p.round_rect(rect, 14.0, ink.wash(0.06 * t));
            }
            let (cx, cy) = (rect.x + 12.0 + 13.0, rect.y + rect.h / 2.0);
            if go {
                p.icon(Icon::Search, cx, cy, 15.0, fade(ink.text()));
            } else {
                let icon = match row.kind {
                    Suggest::Tab(j) => self.tabs.get(j).and_then(|tab| tab.favicon()),
                    _ => None,
                };
                match icon {
                    Some(icon) => p.favicon(icon, cx, cy, 26.0, 13.0, 16.0, t),
                    None => {
                        p.circle(cx, cy, 13.0, fade(rgb(250, 250, 250)));
                        p.text_centered(fonts, &row.mark.to_string(), cx, cy + 4.0, 11.5, fade(rgb(17, 17, 17)), true);
                    }
                }
            }
            let aside_w = fonts.advance(&row.aside, 12.0, false, false).min(160.0);
            let mut right = rect.x + rect.w - 12.0;
            if go {
                // the Enter key cap
                let cap = Rect { x: right - 22.0, y: cy - 10.0, w: 22.0, h: 20.0 };
                p.round_stroke(cap, 6.0, 1.0, ink.wash(0.3 * t));
                p.text_centered(fonts, "↵", cap.x + 11.0, cy + 4.0, 11.0, fade(ink.dim()), false);
                right = cap.x - 10.0;
            }
            let aside = fit(fonts, &row.aside, 12.0, false, 160.0);
            p.text(fonts, &aside, right - aside_w, cy + 4.0, 12.0, fade(ink.dim()), false);
            let title_room = right - aside_w - 12.0 - (cx + 13.0 + 12.0);
            let title = fit(fonts, &row.title, 14.0, false, title_room);
            p.text(fonts, &title, cx + 13.0 + 12.0, cy + 5.0, 14.0, fade(ink.text()), !go);
        }
    }

    /// All tabs at once: a grid of page cards, the tab you are on outlined,
    /// a × on each; a bar with "new tab" and "burn all" at the bottom.
    fn draw_overview(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let t = smooth(self.overview_open);
        if t <= 0.01 {
            return;
        }
        // the backdrop follows the page: light over a light page, dark over a dark one
        let page = p.ink_over(g.viewport);
        let ground = mix(rgb(11, 11, 12), rgb(233, 231, 227), page.t);
        p.round_rect(Rect { x: 0.0, y: 0.0, w: self.w, h: self.h }, 0.0, alpha(ground, t));
        let ink = Ink { t: page.t };
        let fade = |c: Color| alpha(c, t);
        let o = self.overview_layout();
        let left = o.cards.first().map(|r| r.x).unwrap_or(48.0);
        let right = o.cards.iter().map(|r| r.x + r.w).fold(left, f32::max).max(self.w - left);
        p.text(fonts, "Вкладки", left, o.header_y, 26.0, fade(ink.text()), true);
        let count = self.tabs.len().to_string();
        let cw = fonts.advance("Вкладки ", 26.0, true, false);
        p.text(fonts, &count, left + cw, o.header_y, 26.0, fade(ink.dim()), false);
        let hint = "Нажмите, чтобы открыть · ⌘W — закрыть · Esc — назад";
        let hw = fonts.advance(hint, 12.5, false, false);
        if left + cw + 60.0 + hw < right {
            p.text(fonts, hint, right - hw, o.header_y, 12.5, fade(ink.dim()), false);
        }
        let card_bg = mix(rgb(28, 28, 30), rgb(247, 246, 244), page.t);
        for (i, (r, tab)) in o.cards.iter().zip(&self.tabs).enumerate() {
            let active = i == self.active;
            let hot = matches!(self.hover, Hit::OverviewTab(j) | Hit::OverviewClose(j) if j == i);
            p.shadow(*r, 18.0, 22.0, 8.0, (if hot { 0.32 } else { 0.18 }) * t);
            match &tab.thumb {
                Some(img) => {
                    let pose = CardPose {
                        cx: r.x + r.w / 2.0,
                        bottom: r.y + r.h,
                        w: r.w,
                        h: r.h,
                        tilt_deg: 0.0,
                        depth: 0.0,
                        bright: 1.0,
                        border: alpha(ink.text(), 0.12),
                        clip: (0.0, self.w),
                    };
                    p.card(&pose, &Face::Thumb(img));
                }
                None => {
                    p.round_rect(*r, 18.0, fade(card_bg));
                    let bar = |p: &mut Painter, y: f32, w: f32, h: f32, a: f32| {
                        p.round_rect(Rect { x: r.x + 18.0, y: r.y + y, w: (r.w - 36.0) * w, h }, h / 2.0, ink.wash(a * t));
                    };
                    bar(p, 20.0, 0.54, 12.0, 0.16);
                    bar(p, 42.0, 0.92, 7.0, 0.10);
                    bar(p, 56.0, 0.86, 7.0, 0.10);
                }
            }
            if active {
                let ring = Rect { x: r.x - 4.0, y: r.y - 4.0, w: r.w + 8.0, h: r.h + 8.0 };
                p.round_stroke(ring, 21.0, 2.0, fade(ink.text()));
            }
            // the ×: glass over the page thumbnail
            let (bx, by) = overview_close(r);
            let close = Rect { x: bx - 14.0, y: by - 14.0, w: 28.0, h: 28.0 };
            let cink = p.glass(close, 14.0, 10.0, 0.12, t);
            if self.hover == Hit::OverviewClose(i) {
                p.circle(bx, by, 14.0, alpha(cink.text(), t));
                p.icon(Icon::Close, bx, by, 11.0, alpha(cink.inverse(), t));
            } else {
                p.icon(Icon::Close, bx, by, 11.0, alpha(cink.text(), t));
            }
            // the label under the card
            let (lx, ly) = (r.x + 4.0, r.y + r.h + 22.0);
            if let Some(icon) = tab.favicon() {
                p.favicon(icon, lx + 12.0, ly, 24.0, 12.0, 15.0, t);
            } else {
                p.circle(lx + 12.0, ly, 12.0, fade(if active { ink.text() } else { ink.wash(0.14) }));
                let fg = if active { ink.inverse() } else { ink.text() };
                p.text_centered(fonts, &tab.monogram().to_string(), lx + 12.0, ly + 4.0, 11.0, fade(fg), true);
            }
            let room = r.w - 40.0;
            p.text(fonts, &fit(fonts, &tab.title(), 14.0, true, room), lx + 34.0, ly - 2.0, 14.0, fade(ink.text()), true);
            let host = tab.host();
            if !host.is_empty() {
                p.text(fonts, &fit(fonts, &host, 12.0, false, room), lx + 34.0, ly + 14.0, 12.0, fade(ink.dim()), false);
            }
        }
        // the bar of two buttons
        let bink = p.glass(o.bar, o.bar.h / 2.0, 22.0, 0.16, t);
        let hot_new = self.hover == Hit::OverviewNew;
        p.round_rect(o.new_tab, 18.0, alpha(bink.text(), t * if hot_new { 0.85 } else { 1.0 }));
        p.text_centered(fonts, text::NEW_TAB, o.new_tab.x + o.new_tab.w / 2.0, o.new_tab.y + 23.0, 13.0, alpha(bink.inverse(), t), false);
        if self.hover == Hit::OverviewBurn {
            p.round_rect(o.burn, 18.0, bink.wash(0.12 * t));
        }
        p.text_centered(fonts, "Сжечь все", o.burn.x + o.burn.w / 2.0, o.burn.y + 23.0, 13.0, alpha(bink.text(), t), false);
    }

    /// The open capsule while finding: the query, a caret, and the
    /// "2 / 7" counter at the right end.
    fn draw_find_field(&self, p: &mut Painter, fonts: &Fonts, c: Rect, my: f32, ink: Ink, t: f32) {
        let text_x = c.x + 46.0;
        let mut right = c.x + c.w - 16.0;
        if let Some((current, total)) = self.find_status().filter(|_| !self.input.trim().is_empty()) {
            let label = format!("{current} / {total}");
            let w = fonts.advance(&label, 12.0, true, false);
            let pill = Rect { x: right - w - 12.0, y: my - 10.0, w: w + 16.0, h: 20.0 };
            let tone = if total == 0 { ink.dim() } else { ink.text() };
            p.round_rect(pill, 10.0, ink.wash(0.10 * t));
            p.text(fonts, &label, pill.x + 8.0, my + 4.0, 12.0, alpha(tone, t), true);
            right = pill.x - 8.0;
        }
        let room = right - text_x;
        if room < 24.0 {
            return;
        }
        let shown = fit_tail(fonts, &self.input, 14.0, room - 4.0);
        if self.replace_input && !shown.is_empty() {
            let sw = fonts.advance(&shown, 14.0, false, false);
            p.round_rect(Rect { x: text_x - 3.0, y: my - 10.0, w: sw + 6.0, h: 20.0 }, 5.0, ink.wash(0.16 * t));
        }
        p.text(fonts, &shown, text_x, my + 5.0, 14.0, alpha(ink.text(), t), false);
        let caret_x = text_x + fonts.advance(&shown, 14.0, false, false) + 1.0;
        p.round_rect(Rect { x: caret_x, y: my - 8.0, w: 1.5, h: 16.0 }, 0.75, alpha(ink.text(), t));
        if self.input.is_empty() {
            p.text(fonts, text::FIND_PLACEHOLDER, text_x + 6.0, my + 5.0, 14.0, alpha(ink.dim(), t), false);
        }
    }

    fn draw_gestures(&self, p: &mut Painter, g: &Geom) {
        let v = g.viewport;
        // a glass bubble that fills with ink once releasing would act
        let bubble = |p: &mut Painter, x: f32, y: f32, r: f32, progress: f32, ready: bool| -> Ink {
            let rect = Rect { x: x - r, y: y - r, w: r * 2.0, h: r * 2.0 };
            let ink = p.glass(rect, r, 10.0, 0.3, progress.max(0.6));
            if ready {
                p.circle(x, y, r, ink.text());
            }
            ink
        };
        if self.swipe.abs() > 0.02 {
            let back = self.swipe > 0.0;
            let progress = self.swipe.abs().min(1.0);
            let ready = self.swipe.abs() >= 1.0;
            let travel = 56.0 * smooth(progress);
            let x = if back { v.x - 22.0 + travel } else { v.x + v.w + 22.0 - travel };
            let y = v.y + v.h / 2.0;
            let ink = bubble(p, x, y, 22.0, progress, ready);
            let icon = if back { Icon::Left } else { Icon::Right };
            p.icon(icon, x, y, 20.0, if ready { ink.inverse() } else { alpha(ink.text(), progress) });
        }
        if self.pull > 0.02 {
            let progress = self.pull.min(1.0);
            let ready = self.pull >= 1.0;
            // below the capsule, never over it
            let (x, y) = (v.x + v.w / 2.0, v.y + 12.0 + 36.0 * smooth(progress));
            let ink = bubble(p, x, y, 18.0, progress, ready);
            if ready {
                p.icon(Icon::Reload, x, y, 20.0, ink.inverse());
            } else {
                p.arc(x, y, 8.0, -std::f32::consts::FRAC_PI_2, progress * 6.0, 1.8, ink.text());
            }
        }
    }

    /// The settings sheet: a glass panel over a dimmed window, browsers on
    /// the left, countries in a grid on the right; the choice is a solid pill.
    /// The ⋯ button in the top right corner and, when open, its menu.
    fn draw_menu(&self, p: &mut Painter, fonts: &Fonts) {
        let m = self.menu_layout();
        let t = smooth(self.menu_open);
        let b = m.button;
        let hot = self.hover == Hit::Menu;
        let ink = p.glass(b, b.h / 2.0, 10.0, 0.10, if hot || self.menu { 1.0 } else { 0.8 });
        if t > 0.01 {
            p.round_rect(b, b.h / 2.0, alpha(ink.text(), t));
        }
        let dot = mix(ink.text(), ink.inverse(), t);
        for k in -1..=1 {
            p.circle(b.x + b.w / 2.0 + k as f32 * 5.0, b.y + b.h / 2.0, 1.6, dot);
        }
        if t <= 0.01 {
            return;
        }
        let r = m.panel;
        let ink = p.glass(r, 18.0, 24.0, 0.22, t);
        let fade = |c: Color| alpha(c, t);

        // header: who the sites see right now
        let h = m.header;
        p.round_rect(h, 12.0, ink.wash(0.08 * t));
        let now = crate::persona::current();
        let transport = Transport::of_tab(self.transport, self.tab());
        let (cx, cy) = (h.x + 12.0 + 18.0, h.y + h.h / 2.0);
        p.circle(cx, cy, 18.0, fade(ink.text()));
        p.text_centered(fonts, now.country.code, cx, cy + 4.5, 12.0, fade(ink.inverse()), true);
        let tx = cx + 18.0 + 12.0;
        let max = h.x + h.w - 12.0 - tx;
        p.text(fonts, &fit(fonts, now.browser.label, 13.5, true, max), tx, cy - 3.0, 13.5, fade(ink.text()), true);
        let sub = format!("{} · {}", now.country.label, transport.label());
        p.text(fonts, &fit(fonts, &sub, 12.0, false, max), tx, cy + 14.0, 12.0, fade(ink.dim()), false);

        let host = self.tab().host();
        let mut group = MENU[0].group();
        for (i, (rect, item)) in m.items.iter().zip(MENU).enumerate() {
            if item.group() != group {
                group = item.group();
                let line = Rect { x: rect.x + 10.0, y: rect.y - 5.0, w: rect.w - 20.0, h: 1.0 };
                p.round_rect(line, 0.5, ink.wash(0.12 * t));
            }
            let ready = item.ready();
            if ready && self.hover == Hit::MenuItem(i) {
                p.round_rect(*rect, 10.0, ink.wash(0.10 * t));
            }
            let tone = fade(if ready { ink.text() } else { ink.dim() });
            let aside = match item {
                _ if !ready => text::SOON.to_string(),
                MenuItem::Persona => "⌘,".to_string(),
                MenuItem::Traffic => transport.label().to_string(),
                MenuItem::Passport => host.clone(),
                _ => String::new(),
            };
            let aw = if aside.is_empty() { 0.0 } else { fonts.advance(&aside, 12.0, false, false).min(110.0) };
            let label = fit(fonts, item.label(), 13.5, false, rect.w - 36.0 - aw);
            p.text(fonts, &label, rect.x + 12.0, rect.y + rect.h / 2.0 + 4.5, 13.5, tone, false);
            if !aside.is_empty() {
                let aside = fit(fonts, &aside, 12.0, false, 110.0);
                let aw = fonts.advance(&aside, 12.0, false, false);
                p.text(fonts, &aside, rect.x + rect.w - 12.0 - aw, rect.y + rect.h / 2.0 + 4.0, 12.0, fade(ink.dim()), false);
            }
        }
    }

    /// A sheet opened from the ⋯ menu: how the traffic goes, or the site passport.
    fn draw_panel(&self, p: &mut Painter, fonts: &Fonts) {
        let t = smooth(self.panel_open);
        if t <= 0.01 {
            return;
        }
        p.round_rect(Rect { x: 0.0, y: 0.0, w: self.w, h: self.h }, 0.0, alpha(rgb(0, 0, 0), 0.28 * t));
        let panel = self.panel_shown;
        let r = self.panel_rect(panel);
        let ink = p.glass(r, 26.0, 24.0, 0.22, t);
        let fade = |c: Color| alpha(c, t);
        let pad = 24.0;
        let inner = r.w - pad * 2.0;
        let tab = self.tab();
        let transport = Transport::of_tab(self.transport, tab);
        let now = crate::persona::current();
        // key: value rows, the value column starting at `split`
        let facts = |p: &mut Painter, rows: &[(&str, String)], top: f32, split: f32| {
            for (k, (key, value)) in rows.iter().enumerate() {
                let y = top + k as f32 * 30.0;
                p.text(fonts, key, r.x + pad, y, 12.5, fade(ink.dim()), false);
                let value = fit(fonts, value, 13.0, false, inner - split);
                p.text(fonts, &value, r.x + pad + split, y, 13.0, fade(ink.text()), false);
            }
        };
        match panel {
            Panel::Traffic => {
                p.text(fonts, text::TRAFFIC_TITLE, r.x + pad, r.y + 38.0, 18.0, fade(ink.text()), true);
                let mut sub = transport.describe();
                if let Some(first) = sub.get(..2) {
                    // "через Tor" → "Через Tor": the line starts a sentence here
                    let upper: String = first.chars().flat_map(char::to_uppercase).collect();
                    sub = format!("{upper}{}", &sub[2..]);
                }
                p.text(fonts, &fit(fonts, &sub, 12.5, false, inner), r.x + pad, r.y + 59.0, 12.5, fade(ink.dim()), false);

                // the route: you → the hops → the site
                let host = tab.host();
                let mut nodes: Vec<(String, String)> = vec![("Вы".into(), "этот компьютер".into())];
                match transport {
                    Transport::Tor => {
                        for (k, name) in ["вход", "середина", "выход"].iter().enumerate() {
                            nodes.push(((k + 1).to_string(), format!("Tor: {name}")));
                        }
                    }
                    Transport::I2p => nodes.push(("I2P".into(), "туннели I2P".into())),
                    Transport::Chain(n) => {
                        for k in 1..=n.min(4) {
                            nodes.push((k.to_string(), "прокси".into()));
                        }
                    }
                    Transport::Direct => {}
                }
                let site = if host.is_empty() { "сайт".to_string() } else { host };
                nodes.push((tab.monogram().to_string(), site));
                let n = nodes.len();
                let y = r.y + 124.0;
                let x0 = r.x + pad + 34.0;
                let step = (inner - 68.0) / (n - 1) as f32;
                for k in 0..n - 1 {
                    let line = Rect { x: x0 + k as f32 * step + 24.0, y: y - 1.0, w: step - 48.0, h: 2.0 };
                    p.round_rect(line, 1.0, ink.wash(0.35 * t));
                }
                for (k, (mark, name)) in nodes.iter().enumerate() {
                    let cx = x0 + k as f32 * step;
                    let ends = k == 0 || k == n - 1;
                    p.circle(cx, y, 20.0, if ends { fade(ink.text()) } else { ink.wash(0.16 * t) });
                    let tone = fade(if ends { ink.inverse() } else { ink.text() });
                    p.text_centered(fonts, mark, cx, y + 4.5, 12.5, tone, true);
                    let name = fit(fonts, name, 11.5, false, step.min(150.0) - 8.0);
                    p.text_centered(fonts, &name, cx, y + 40.0, 11.5, fade(ink.dim()), false);
                }

                let dns = match transport {
                    Transport::Direct if self.doh => "DoH: имена зашифрованы",
                    Transport::Direct => "системный: провайдер видит имена",
                    Transport::Tor | Transport::I2p => "внутри сети, имена не уходят наружу",
                    Transport::Chain(_) => "разрешает прокси, не этот компьютер",
                };
                let rows = [
                    ("DNS", dns.to_string()),
                    ("Сайт видит", format!("{}, {}", now.browser.label, now.country.label)),
                    ("Язык и время", format!("{} · {}", now.country.language, now.country.time_zone)),
                ];
                facts(p, &rows, r.y + 214.0, 130.0);
                let foot = fit(fonts, text::TRAFFIC_FOOT, 11.5, false, inner);
                p.text(fonts, &foot, r.x + pad, r.y + r.h - 20.0, 11.5, fade(ink.dim()), false);
            }
            Panel::Passport => {
                p.text(fonts, text::PASSPORT_TITLE, r.x + pad, r.y + 30.0, 11.5, fade(ink.dim()), true);
                let Some(loaded) = self.active_loaded() else {
                    p.text(fonts, &fit(fonts, text::PASSPORT_EMPTY, 14.0, false, inner), r.x + pad, r.y + 62.0, 14.0, fade(ink.text()), false);
                    return;
                };
                let url = &loaded.url;
                let host = url.host.trim_start_matches("www.");
                p.text(fonts, &fit(fonts, host, 20.0, true, inner), r.x + pad, r.y + 56.0, 20.0, fade(ink.text()), true);
                if !loaded.title.trim().is_empty() {
                    p.text(fonts, &fit(fonts, loaded.title.trim(), 12.5, false, inner), r.x + pad, r.y + 77.0, 12.5, fade(ink.dim()), false);
                }
                let crypto = match url.scheme.as_str() {
                    "https" | "gemini" => "TLS: по пути не прочитать",
                    _ if url.needs_overlay().is_some() => "шифрует сама сеть",
                    _ => "нет: по пути видно всё",
                };
                let status = if loaded.status == 0 { "—".to_string() } else { loaded.status.to_string() };
                let cells = [
                    ("Протокол", url.scheme.to_uppercase()),
                    ("Ответ сервера", status),
                    ("Шифрование", crypto.to_string()),
                    ("Маршрут", transport.label().to_string()),
                    ("Скрипты", loaded.scripts.to_string()),
                    ("Правила CSS", loaded.rules.to_string()),
                    ("Сайт видит браузер", now.browser.label.to_string()),
                    ("Язык и время", format!("{} · {}", now.country.language, now.country.time_zone)),
                ];
                let col = (inner - 12.0) / 2.0;
                for (k, (key, value)) in cells.iter().enumerate() {
                    let cell = Rect { x: r.x + pad + (k % 2) as f32 * (col + 12.0), y: r.y + 96.0 + (k / 2) as f32 * 52.0, w: col, h: 44.0 };
                    p.round_rect(cell, 12.0, ink.wash(0.06 * t));
                    p.text(fonts, key, cell.x + 12.0, cell.y + 17.0, 11.0, fade(ink.dim()), false);
                    let value = fit(fonts, value, 13.0, true, cell.w - 24.0);
                    p.text(fonts, &value, cell.x + 12.0, cell.y + 35.0, 13.0, fade(ink.text()), true);
                }
                if !loaded.note.trim().is_empty() {
                    let note = fit(fonts, loaded.note.trim(), 12.0, false, inner);
                    p.text(fonts, &note, r.x + pad, r.y + 318.0, 12.0, fade(ink.dim()), false);
                }
                let foot = fit(fonts, text::PASSPORT_FOOT, 11.5, false, inner);
                p.text(fonts, &foot, r.x + pad, r.y + r.h - 20.0, 11.5, fade(ink.dim()), false);
            }
        }
    }

    fn draw_settings(&self, p: &mut Painter, fonts: &Fonts) {
        let t = smooth(self.settings_open);
        if t <= 0.01 {
            return;
        }
        p.round_rect(Rect { x: 0.0, y: 0.0, w: self.w, h: self.h }, 0.0, alpha(rgb(0, 0, 0), 0.28 * t));
        let sheet = self.settings_layout();
        let r = sheet.panel;
        let ink = p.glass(r, 26.0, 24.0, 0.22, t);
        let fade = |c: Color| alpha(c, t);
        let pad = 24.0;
        p.text(fonts, text::SETTINGS_TITLE, r.x + pad, r.y + 38.0, 18.0, fade(ink.text()), true);
        p.text(fonts, text::SETTINGS_SUB, r.x + pad, r.y + 59.0, 12.5, fade(ink.dim()), false);
        let label_y = r.y + 72.0 + 16.0;
        p.text(fonts, text::SETTINGS_BROWSER, r.x + pad, label_y, 11.5, fade(ink.dim()), true);
        p.text(fonts, text::SETTINGS_COUNTRY, sheet.divider + 18.0, label_y, 11.5, fade(ink.dim()), true);
        let top = r.y + 72.0 + 26.0;
        p.round_rect(Rect { x: sheet.divider, y: top, w: 1.0, h: r.y + r.h - 48.0 - top }, 0.5, ink.wash(0.14 * t));

        let now = crate::persona::current();
        let row = |p: &mut Painter, rect: Rect, label: &str, aside: &str, chosen: bool, hot: bool| {
            if chosen {
                p.round_rect(rect, rect.h / 2.0, fade(ink.text()));
            } else if hot {
                p.round_rect(rect, rect.h / 2.0, ink.wash(0.10 * t));
            }
            let tone = fade(if chosen { ink.inverse() } else { ink.text() });
            let label = fit(fonts, label, 13.0, chosen, rect.w - 46.0);
            p.text(fonts, &label, rect.x + 12.0, rect.y + rect.h / 2.0 + 4.5, 13.0, tone, chosen);
            if !aside.is_empty() {
                let aw = fonts.advance(aside, 10.5, true, false);
                let dim = fade(if chosen { alpha(ink.inverse(), 0.6) } else { ink.dim() });
                p.text(fonts, aside, rect.x + rect.w - 12.0 - aw, rect.y + rect.h / 2.0 + 4.0, 10.5, dim, true);
            }
        };
        for (i, (rect, b)) in sheet.browsers.iter().zip(crate::persona::BROWSERS).enumerate() {
            row(p, *rect, b.label, "", b.id == now.browser.id, self.hover == Hit::SettingsBrowser(i));
        }
        for (i, (rect, c)) in sheet.countries.iter().zip(crate::persona::COUNTRIES).enumerate() {
            row(p, *rect, c.label, c.code, c.code == now.country.code, self.hover == Hit::SettingsCountry(i));
        }
        p.text(fonts, text::SETTINGS_FOOT, r.x + pad, r.y + r.h - 20.0, 11.5, fade(ink.dim()), false);
    }

    fn draw_toast(&self, p: &mut Painter, fonts: &Fonts, g: &Geom) {
        let Some((message, left)) = &self.toast else { return };
        let t = (left / 0.3).min(1.0);
        let w = fonts.advance(message, 13.0, false, false) + 32.0;
        let r = Rect { x: (self.w - w) / 2.0, y: (g.dock.y - 52.0).min(self.h - 58.0), w, h: 34.0 };
        let ink = p.glass(r, 17.0, 14.0, 0.16, t);
        p.text_centered(fonts, message, r.x + w / 2.0, r.y + 22.0, 13.0, alpha(ink.text(), t), false);
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
    sessions: HashMap<(u64, u64), mpsc::Sender<PageEvent>>,
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
    /// Save a URL to the downloads directory; the bytes take the same network
    /// path as the page they were linked from.
    fn download(&mut self, url: Url) {
        let name = download_name(&url);
        let dir = downloads_dir();
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.ui.toast(&format!("{}: {err}", text::SAVED));
            return;
        }
        let timeout = Duration::from_secs(60);
        match crate::net::fetch_with(&url, &self.chain, self.resolver.as_ref(), timeout) {
            Ok(response) => {
                let path = unique_path(&dir, &name);
                match std::fs::write(&path, &response.body) {
                    Ok(()) => {
                        let shown = path.file_name().and_then(|n| n.to_str()).unwrap_or(&name).to_string();
                        self.ui.toast(&format!("{}: {shown}", text::SAVED));
                    }
                    Err(err) => self.ui.toast(&format!("{}: {err}", text::SAVED)),
                }
            }
            Err(err) => self.ui.toast(&format!("Не скачалось: {err}")),
        }
        self.window.request_redraw();
    }

    /// A link that points at a file (not a page) should save, not navigate.
    fn looks_like_file(url: &Url) -> bool {
        let path = url.path.to_ascii_lowercase();
        let Some(name) = path.rsplit('/').next() else { return false };
        let Some((_, ext)) = name.rsplit_once('.') else { return false };
        const FILES: &[&str] = &[
            "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "dmg", "pkg", "deb", "rpm", "apk",
            "exe", "msi", "iso", "pdf", "epub", "mobi", "doc", "docx", "xls", "xlsx", "ppt",
            "pptx", "odt", "csv", "json", "xml", "txt", "md", "mp3", "flac", "ogg", "wav",
            "mp4", "mkv", "webm", "avi", "mov", "png", "jpg", "jpeg", "gif", "webp", "svg",
            "torrent", "sig", "asc", "bin", "img",
        ];
        FILES.contains(&ext)
    }

    fn save_current(&mut self) {
        match self.ui.active_loaded().map(|l| l.url.clone()) {
            Some(url) => self.download(url),
            None => self.ui.toast("Нечего сохранять"),
        }
    }

    fn spawn(&mut self, load: Load) {
        let fonts = self.fonts.clone();
        let chain = self.chain.clone();
        let resolver = self.resolver.clone();
        let proxy = self.proxy.clone();
        let viewport = self.ui.viewport();
        let (width, height) = (viewport.w, viewport.h);
        let (events, inbox) = mpsc::channel::<PageEvent>();
        // pages kept for back/forward keep their threads; dropping a sender ends one
        self.sessions.insert((load.tab_id, load.nav), events);
        self.prune_sessions();
        self.field = None;
        crate::js::spawn_page(move || {
            // show the page as soon as HTML, CSS and scripts are in; images follow
            let opened = page::open(&load.url, &chain, resolver.as_ref(), width, &fonts, Duration::from_secs(20), false, true, load.form.as_deref());
            let (result, session, late) = match opened {
                Ok((mut loaded, mut session, late)) => {
                    if let Some(session) = session.as_mut() {
                        let outcome = session.handle(PageEvent::Resize { width, height }, &fonts);
                        if let Some(page) = outcome.page { loaded.page = page; }
                        if let Some(title) = outcome.title { loaded.title = title; }
                    }
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
                loop {
                    let event = match session.next_timer_delay() {
                        Some(delay) => match inbox.recv_timeout(delay) {
                            Ok(event) => event,
                            Err(mpsc::RecvTimeoutError::Timeout) => PageEvent::Tick,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        },
                        None => match inbox.recv() { Ok(event) => event, Err(_) => break },
                    };
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
        match self.sessions.get(&(tab_id, nav)) {
            Some(tx) => tx.send(event).is_ok(),
            None => false,
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
        if let Some(fragment) = outcome.jump.as_deref() {
            self.ui.jump_to(fragment);
        }
        if let Some(target) = outcome.navigate {
            let base = self.ui.active_loaded().map(|l| l.url.clone());
            if let Some(url) = base.and_then(|b| b.resolve(&target)).or_else(|| Url::parse(&target)) {
                if self.ui.jump(&url) {
                    self.update_title();
                    return;
                }
                let mut load = self.ui.navigate(url);
                load.form = outcome.form;
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
        match load {
            Some(load) => self.spawn(load),
            // restored from a snapshot
            None => {
                self.field = None;
                self.prune_sessions();
                let viewport = self.ui.viewport();
                self.to_page(PageEvent::Resize { width: viewport.w, height: viewport.h });
                self.update_title();
                self.window.request_redraw();
            }
        }
    }

    /// End the script threads of pages no tab shows, loads or keeps.
    fn prune_sessions(&mut self) {
        let kept = self.ui.kept_navs();
        self.sessions.retain(|key, _| kept.contains(key));
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.physical = (width, height);
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            let _ = self.surface.resize(w, h);
        }
        self.ui.set_size(width as f32 / self.scale, height as f32 / self.scale);
        let viewport = self.ui.viewport();
        // Sessions stay on their workers; each rebuilds layout and hit boxes.
        // Include background/history workers so restored tabs use this viewport.
        for session in self.sessions.values() {
            let _ = session.send(PageEvent::Resize { width: viewport.w, height: viewport.h });
        }
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
        if self.ui.dragging() {
            self.ui.drag_to(x);
            return;
        }
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

    /// Present as another browser or country: remembered by `persona`, and
    /// the open page is loaded again so the site sees the change.
    fn choose_persona(&mut self, browser: &str, country: &str) {
        let before = crate::persona::current();
        if before.browser.id == browser && before.country.code == country {
            return;
        }
        match crate::persona::choose(browser, country) {
            Ok(now) => {
                self.ui.toast(&format!("Сайты видят: {}, {}", now.browser.label, now.country.label));
                if self.ui.active_loaded().is_some() {
                    let load = self.ui.reload();
                    self.run(load);
                }
            }
            Err(err) => self.ui.toast(&format!("Не удалось: {err}")),
        }
    }

    fn click(&mut self, x: f32, y: f32) {
        let hit = self.ui.hit(x, y);
        if self.ui.settings {
            match hit {
                Hit::SettingsBrowser(i) => {
                    let country = crate::persona::current().country.code;
                    self.choose_persona(crate::persona::BROWSERS[i].id, country);
                }
                Hit::SettingsCountry(i) => {
                    let browser = crate::persona::current().browser.id;
                    self.choose_persona(browser, crate::persona::COUNTRIES[i].code);
                }
                Hit::Settings => {}
                _ => self.ui.close_settings(),
            }
            self.window.request_redraw();
            return;
        }
        if self.ui.panel.is_some() {
            // modal: a click inside does nothing, outside closes it
            if hit != Hit::Panel {
                self.ui.close_panel();
            }
            self.window.request_redraw();
            return;
        }
        let mut hit = hit;
        if self.ui.menu || hit == Hit::Menu {
            match hit {
                Hit::Menu => self.ui.toggle_menu(),
                Hit::MenuItem(i) => {
                    if MENU.get(i) == Some(&MenuItem::Burn) {
                        // burning is the burn button's job, below
                        self.ui.close_menu();
                        hit = Hit::Burn;
                    } else {
                        self.ui.pick_menu(i);
                    }
                }
                Hit::Panel => {}
                _ => self.ui.close_menu(),
            }
            if hit != Hit::Burn {
                self.window.request_redraw();
                return;
            }
        }
        if self.ui.overview && hit != Hit::Burn {
            match hit {
                Hit::OverviewTab(i) => {
                    self.ui.select(i);
                    self.ui.close_overview();
                }
                Hit::OverviewClose(i) => {
                    self.ui.close_tab(i);
                    let ui = &self.ui;
                    self.sessions.retain(|(id, _), _| ui.has_tab(*id));
                }
                Hit::OverviewNew => {
                    self.ui.close_overview();
                    self.ui.new_tab();
                }
                Hit::OverviewBurn => {
                    // burning is the burn button's job, below
                    self.ui.close_overview();
                    hit = Hit::Burn;
                }
                Hit::Panel => {}
                _ => self.ui.close_overview(),
            }
            if hit != Hit::Burn {
                self.update_title();
                self.window.request_redraw();
                return;
            }
        }
        if hit != Hit::Capsule && !matches!(hit, Hit::Suggest(_)) && self.ui.editing {
            // closing a find field keeps its highlights (⌘G steps on, Esc clears)
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
            // selecting happens on release, so the card can be dragged instead
            Hit::Tab(i) => self.ui.press_tab(i, x),
            Hit::TabClose(i) => {
                self.ui.close_tab(i);
                let ui = &self.ui;
                self.sessions.retain(|(id, _), _| ui.has_tab(*id));
            }
            Hit::Burn => {
                self.ui.burn();
                self.page_cache = None;
                self.sessions.clear();
                self.field = None;
                // nothing of the past session stays: open connections, DNS answers
                crate::net::forget_connections();
                crate::resolver::forget();
                crate::cookies::forget();
            }
            Hit::Page => {
                // pages with scripts get the click; they decide what happens
                let new_tab = self.mods.super_key() || self.mods.control_key();
                if let Some((node, href)) = self.element_at(x, y).filter(|_| !new_tab) {
                    self.field = None;
                    if self.to_page(PageEvent::Click { node, href }) {
                        return;
                    }
                }
                if let Some(href) = self.link_at(x, y) {
                    if let Some(url) = Url::parse(&href) {
                        if self.mods.alt_key() || App::looks_like_file(&url) {
                            self.download(url);
                        } else if self.mods.super_key() || self.mods.control_key() {
                            // ⌘-click: a background tab
                            let load = self.ui.open_in_background(url);
                            self.spawn(load);
                        } else if !self.ui.jump(&url) {
                            let load = self.ui.navigate(url);
                            self.spawn(load);
                        }
                    }
                }
            }
            Hit::Bookmark(i) => {
                if let Some((_, url)) = self.ui.bookmarks.get(i) {
                    if let Some(url) = Url::parse(url) {
                        self.ui.editing = false;
                        let load = self.ui.navigate(url);
                        self.spawn(load);
                    }
                }
            }
            Hit::Transport => self.ui.open_settings(),
            Hit::Dock
            | Hit::Nothing
            | Hit::Settings
            | Hit::SettingsBrowser(_)
            | Hit::SettingsCountry(_)
            | Hit::Menu
            | Hit::MenuItem(_)
            | Hit::Panel
            | Hit::OverviewTab(_)
            | Hit::OverviewClose(_)
            | Hit::OverviewNew
            | Hit::OverviewBurn => {}
            Hit::Suggest(i) => {
                let load = self.ui.pick_suggestion(i);
                self.run(load);
            }
        }
        self.update_title();
    }

    fn key(&mut self, key: &Key, typed: Option<&str>) -> bool {
        let cmd = self.mods.super_key() || self.mods.control_key();
        if cmd && matches!(key, Key::Character(c) if c.as_str() == "," || c.as_str() == "б") {
            if self.ui.settings {
                self.ui.close_settings();
            } else {
                self.ui.open_settings();
            }
            return true;
        }
        if self.ui.settings && !cmd {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.ui.close_settings();
            }
            return true;
        }
        if cmd && matches!(key, Key::Character(c) if c.as_str() == "\\" || c.as_str() == "|") {
            // ⌘⇧\ like Safari: all tabs
            if self.ui.overview {
                self.ui.close_overview();
            } else {
                self.ui.open_overview();
            }
            return true;
        }
        if (self.ui.menu || self.ui.panel.is_some() || self.ui.overview) && !cmd {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.ui.close_menu();
                self.ui.close_panel();
                self.ui.close_overview();
            }
            return true;
        }
        if cmd {
            match key {
                Key::Character(c) => match c.as_str() {
                    "l" | "д" => self.ui.begin_editing(true),
                    "v" | "м" => {
                        let pasted: String = clipboard_text().chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
                        let pasted = pasted.trim().to_string();
                        if !pasted.is_empty() {
                            if self.ui.editing {
                                self.ui.type_text(&pasted);
                                if self.ui.finding {
                                    self.ui.find_step(&self.fonts, 0);
                                }
                            } else if self.field.is_some() {
                                self.type_into_field(&Key::Character("".into()), Some(&pasted));
                            } else {
                                // pasting with nothing focused: straight into the address field
                                self.ui.begin_editing(false);
                                self.ui.type_text(&pasted);
                            }
                        }
                    }
                    "c" | "с" | "x" | "ч" => {
                        let cut = matches!(c.as_str(), "x" | "ч");
                        let text = if self.ui.editing {
                            self.ui.input_text().to_string()
                        } else if let Some(field) = &self.field {
                            field.value.clone()
                        } else {
                            // nothing selected on a page: the page address
                            self.ui.active_loaded().map(|l| l.url.to_string()).unwrap_or_default()
                        };
                        if !text.is_empty() {
                            set_clipboard_text(&text);
                            if cut && self.ui.editing {
                                self.ui.select_all();
                                self.ui.backspace();
                            }
                        }
                    }
                    "a" | "ф" => {
                        if self.ui.editing {
                            self.ui.select_all();
                        }
                    }
                    "f" | "а" => self.ui.begin_find(),
                    "g" | "п" => {
                        if self.ui.find_status().is_some() {
                            let step = if self.mods.shift_key() { -1 } else { 1 };
                            self.ui.find_step(&self.fonts, step);
                        }
                    }
                    "t" | "е" => self.ui.new_tab(),
                    "w" | "ц" => {
                        self.ui.close_active();
                        let ui = &self.ui;
                        self.sessions.retain(|(id, _), _| ui.has_tab(*id));
                    }
                    "r" | "к" => {
                        let load = self.ui.reload();
                        self.run(load);
                    }
                    "d" | "в" => {
                        self.ui.toggle_bookmark();
                        self.window.request_redraw();
                    }
                    "s" | "ы" => self.save_current(),
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
        if self.ui.editing && self.ui.finding {
            match key {
                Key::Named(NamedKey::Escape) => self.ui.end_find(),
                Key::Named(NamedKey::Enter) => {
                    let step = if self.mods.shift_key() { -1 } else { 1 };
                    self.ui.find_step(&self.fonts, step);
                }
                Key::Named(NamedKey::Backspace) => {
                    self.ui.backspace();
                    self.ui.find_step(&self.fonts, 0);
                }
                _ => {
                    if let Some(text) = typed {
                        self.ui.type_text(text);
                        self.ui.find_step(&self.fonts, 0);
                    }
                }
            }
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
            Key::Named(NamedKey::Escape) if self.ui.find_status().is_some() => self.ui.end_find(),
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
            Hit::Tab(_) | Hit::TabClose(_) | Hit::Dock | Hit::Prev | Hit::Next | Hit::NewTab | Hit::Transport | Hit::Burn => {
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
    let window_builder = WindowBuilder::new();
    // the page runs under a transparent title bar: no grey strip on top,
    // the traffic lights float over the site like the capsule does
    #[cfg(target_os = "macos")]
    let window_builder = {
        use winit::platform::macos::WindowBuilderExtMacOS;
        window_builder.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
    };
    let window = Arc::new(
        window_builder
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
                // A resize may have happened while this navigation was loading.
                let viewport = app.ui.viewport();
                if let Some(session) = app.sessions.get(&(done.tab_id, done.nav)) {
                    let _ = session.send(PageEvent::Resize { width: viewport.w, height: viewport.h });
                }
                app.prune_sessions();
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
                            // the strip under the transparent title bar moves the window
                            MouseButton::Left
                                if y < theme::TITLEBAR
                                    && matches!(app.ui.hit(x, y), Hit::Page | Hit::Nothing)
                                    && app.link_at(x, y).is_none() =>
                            {
                                let _ = app.window.drag_window();
                            }
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
                    WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                        if app.ui.release() {
                            app.update_title();
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
        Loaded { url: Url::parse(url).unwrap(), title: title.into(), page, scripts: 0, status: 200, rules: 0, note: "http 200".into(), icon: None }
    }

    fn open(ui: &mut Ui, url: &str, title: &str) {
        let load = ui.navigate(Url::parse(url).unwrap());
        let fonts = Fonts::load();
        let loaded = fake_loaded(url, title);
        let thumb = thumbnail(&fonts, &loaded.page, 1184.0);
        ui.finish(load.tab_id, load.nav, Ok((loaded, thumb)));
    }

    #[test]
    fn resize_updates_media_queries_script_viewport_and_link_hit_boxes() {
        let fonts = Fonts::load();
        let mut ui = Ui::new(Transport::Direct, true);
        let url = Url::parse("https://resize.example/").unwrap();
        let document = crate::dom::parse_document("<html><body><a id='target' href='/next'>Resize target</a></body></html>");
        let sheet = crate::css::Stylesheet::parse(
            "body { margin: 0 } a { display: block; width: 100px; height: 30px } @media (max-width: 700px) { a { width: 220px; margin-left: 120px } } @media (max-height: 500px) { a { height: 60px } }"
        );
        let scripts = vec![("window.addEventListener('resize', () => { document.title = innerWidth + 'x' + innerHeight; });".into(), "inline script".into())];
        let mut session = crate::js::Session::start(&document, &url, &scripts, None, sheet, crate::images::ImageMap::new(), 1200.0);
        let load = ui.navigate(url.clone());
        let page = session.layout(&fonts);
        let thumb = thumbnail(&fonts, &page, 1200.0);
        ui.finish(load.tab_id, load.nav, Ok((Loaded { url, title: String::new(), page, scripts: 1, status: 200, rules: 3, note: "http 200".into(), icon: None }, thumb)));
        let mut generation = ui.active_page().unwrap().1;
        for (width, height, link_width, link_height) in [(1200.0, 900.0, 100.0, 30.0), (500.0, 400.0, 220.0, 60.0)] {
            ui.set_size(width, height);
            let viewport = ui.viewport();
            let outcome = session.handle(PageEvent::Resize { width: viewport.w, height: viewport.h }, &fonts);
            assert!(outcome.page.is_some(), "resize must produce a new layout");
            assert!(ui.update_page(load.tab_id, load.nav, outcome.page, outcome.title));
            assert!(ui.active_page().unwrap().1 > generation, "render cache must be invalidated");
            generation = ui.active_page().unwrap().1;
            let loaded = ui.active_loaded().unwrap();
            assert_eq!(loaded.title, format!("{}x{}", viewport.w, viewport.h));
            let link = &loaded.page.links[0];
            let rect = loaded.page.boxes.iter()
                .map(|(rect, _)| rect).min_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h))).unwrap_or_else(|| panic!("link {link:?}; boxes {:?}", loaded.page.boxes));
            assert!((rect.w - link_width).abs() < 1.0, "width: {rect:?}");
            assert!((rect.h - link_height).abs() < 1.0, "height: {rect:?}");
            if width < 700.0 { assert!(link.rect.x >= 120.0, "link hit box must move with layout"); }
            let hit = loaded.page.element_at(link.rect.x + 2.0, link.rect.y + 2.0).unwrap();
            assert_eq!(hit.1.as_deref(), Some("/next"));
        }
    }

    #[test]
    fn back_and_forward_come_from_memory() {
        let mut ui = Ui::new(Transport::Direct, true);
        open(&mut ui, "https://a.example/", "A");
        ui.scroll_to(40.0);
        let a_scroll = ui.active_scroll();
        let a_nav = ui.active_live().unwrap().1;
        open(&mut ui, "https://b.example/", "B");
        assert!(ui.back().is_none(), "no load: the page is kept");
        assert_eq!(ui.active_loaded().unwrap().title, "A");
        assert_eq!(ui.active_scroll(), a_scroll, "scroll as it was");
        assert_eq!(ui.active_live().unwrap().1, a_nav, "its script session answers again");
        assert!(ui.kept_navs().len() >= 2, "B is kept too");
        assert!(ui.forward().is_none());
        assert_eq!(ui.active_loaded().unwrap().title, "B");
        // a new page from A drops B (the forward entry) with its snapshot
        assert!(ui.back().is_none());
        open(&mut ui, "https://c.example/", "C");
        assert!(ui.forward().is_none() && ui.active_loaded().unwrap().title == "C", "nothing forward of C");
        assert!(ui.back().is_none());
        assert_eq!(ui.active_loaded().unwrap().title, "A");
        let tab = &ui.tabs[ui.active];
        assert!(tab.snapshots.iter().all(|s| s.loaded.title != "B"), "B is gone");
    }

    #[test]
    fn anchors_scroll_and_find_highlights() {
        let fonts = Fonts::load();
        let mut ui = Ui::new(Transport::Direct, true);
        let sheet = crate::css::Stylesheet::default();
        let long = "<p style='height:3000px'>start needle</p><h2 id='deep'>Deep needle section</h2><p style='height:3000px'>needle tail</p>";
        let page = crate::layout::Layout::new(&fonts, &sheet, 1184.0).build(&crate::dom::parse_document(long));
        let deep = page.anchor("deep").unwrap();
        // a load with #deep lands on it
        let load = ui.navigate(Url::parse("https://a.example/doc#deep").unwrap());
        assert!(!load.url.to_string().contains('#'), "the fragment is not sent: {:?}", load.url);
        let loaded = Loaded { url: load.url.clone(), title: "Doc".into(), page, scripts: 0, status: 200, rules: 0, note: String::new(), icon: None };
        let thumb = thumbnail(&fonts, &loaded.page, 1184.0);
        ui.finish(load.tab_id, load.nav, Ok((loaded, thumb)));
        assert_eq!(ui.active_scroll(), deep);
        // a same-page #link scrolls without loading
        assert!(ui.jump(&Url::parse("https://a.example/doc#top").unwrap()));
        assert_eq!(ui.active_scroll(), 0.0);
        assert!(!ui.jump(&Url::parse("https://b.example/doc#deep").unwrap()), "another page loads");
        // find: three matches, highlighted, Enter walks them, Esc clears
        let draws_before = ui.active_page().unwrap().3.draws.len();
        ui.begin_find();
        ui.type_text("NEEDLE");
        ui.find_step(&fonts, 0);
        assert_eq!(ui.find_status(), Some((1, 3)));
        assert_eq!(ui.active_page().unwrap().3.draws.len(), draws_before + 3);
        ui.find_step(&fonts, 1);
        assert_eq!(ui.find_status(), Some((2, 3)));
        settle(&mut ui);
        dump(&frame(&ui, 2.0), "find");
        assert!(ui.active_scroll() > 2000.0, "the second match is brought into view");
        ui.find_step(&fonts, -1);
        ui.find_step(&fonts, -1);
        assert_eq!(ui.find_status(), Some((3, 3)), "wraps around");
        ui.end_find();
        assert_eq!(ui.find_status(), None);
        assert_eq!(ui.active_page().unwrap().3.draws.len(), draws_before, "highlights removed");
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
        // the pointer rests on the dock, so it stays up
        ui.set_hover(Hit::Dock);
        settle(&mut ui);
        ui
    }

    #[test]
    fn settings_sheet_lists_browsers_and_countries() {
        let mut ui = browsing();
        // the transport chip opens it
        let g = ui.geom();
        assert_eq!(ui.hit(g.chip.x + g.chip.w / 2.0, g.chip.y + g.chip.h / 2.0), Hit::Transport);
        ui.open_settings();
        settle(&mut ui);
        let sheet = ui.settings_layout();
        assert_eq!(sheet.browsers.len(), crate::persona::BROWSERS.len());
        assert_eq!(sheet.countries.len(), crate::persona::COUNTRIES.len());
        let centre = |r: &Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let (x, y) = centre(&sheet.browsers[2]);
        assert_eq!(ui.hit(x, y), Hit::SettingsBrowser(2));
        let (x, y) = centre(&sheet.countries[12]);
        assert_eq!(ui.hit(x, y), Hit::SettingsCountry(12));
        // rows stay inside the panel and do not overlap
        let p = sheet.panel;
        for r in sheet.browsers.iter().chain(&sheet.countries) {
            assert!(r.x >= p.x && r.x + r.w <= p.x + p.w && r.y >= p.y && r.y + r.h <= p.y + p.h, "{r:?} outside {p:?}");
        }
        assert!(sheet.browsers.iter().all(|b| b.x + b.w < sheet.divider));
        // modal: the dock under it does not react; outside is "nothing" (closes)
        assert_eq!(ui.hit(p.x + 4.0, p.y + 4.0), Hit::Settings);
        assert_eq!(ui.hit(4.0, ui.h - 4.0), Hit::Nothing);
        ui.set_hover(Hit::SettingsCountry(3));
        dump(&frame(&ui, 2.0), "settings");
        ui.close_settings();
        ui.set_hover(Hit::Dock);
        settle(&mut ui);
        assert_eq!(ui.hit(g.chip.x + g.chip.w / 2.0, g.chip.y + g.chip.h / 2.0), Hit::Transport);
    }

    #[test]
    fn menu_opens_its_panels_and_closes() {
        let mut ui = browsing();
        let centre = |r: &Rect| (r.x + r.w / 2.0, r.y + r.h / 2.0);
        // the ⋯ button sits in the top right corner, inside the window
        let m = ui.menu_layout();
        assert!(m.button.x + m.button.w <= ui.w && m.button.y >= 0.0);
        let (bx, by) = centre(&m.button);
        assert_eq!(ui.hit(bx, by), Hit::Menu);
        ui.toggle_menu();
        settle(&mut ui);
        let m = ui.menu_layout();
        assert_eq!(m.items.len(), MENU.len());
        for (i, r) in m.items.iter().enumerate() {
            let p = m.panel;
            assert!(r.y >= p.y && r.y + r.h <= p.y + p.h && r.x + r.w <= ui.w, "row {i} outside the menu");
            assert_eq!(ui.hit(centre(r).0, centre(r).1), Hit::MenuItem(i));
        }
        // modal: outside closes, the dock below does not react
        assert_eq!(ui.hit(20.0, ui.h - 20.0), Hit::Nothing);
        ui.set_hover(Hit::MenuItem(2));
        dump(&frame(&ui, 2.0), "menu");

        // a row that is not ready only says so
        let profiles = MENU.iter().position(|i| *i == MenuItem::Profiles).unwrap();
        assert_eq!(ui.pick_menu(profiles), None);
        assert!(ui.menu);

        let traffic = MENU.iter().position(|i| *i == MenuItem::Traffic).unwrap();
        assert_eq!(ui.pick_menu(traffic), Some(MenuItem::Traffic));
        assert!(!ui.menu && ui.panel == Some(Panel::Traffic));
        settle(&mut ui);
        let r = ui.panel_rect(Panel::Traffic);
        assert_eq!(ui.hit(centre(&r).0, centre(&r).1), Hit::Panel);
        assert_eq!(ui.hit(bx, by), Hit::Nothing);
        dump(&frame(&ui, 2.0), "traffic");

        ui.close_panel();
        ui.toggle_menu();
        let passport = MENU.iter().position(|i| *i == MenuItem::Passport).unwrap();
        ui.pick_menu(passport);
        settle(&mut ui);
        assert_eq!(ui.panel, Some(Panel::Passport));
        dump(&frame(&ui, 2.0), "passport");

        let persona = MENU.iter().position(|i| *i == MenuItem::Persona).unwrap();
        ui.close_panel();
        ui.toggle_menu();
        ui.pick_menu(persona);
        assert!(ui.settings && !ui.menu);
    }

    #[test]
    fn site_icon_takes_the_place_of_the_monogram() {
        let mut ui = browsing();
        let host = ui.tab().host();
        // a solid red 16×16 icon
        let red = Arc::new(Bitmap::from_premultiplied(16, 16, [220u8, 30, 40, 255].repeat(256)));
        let i = ui.active;
        ui.tabs[i].set_icon(&host, red);
        let c = frame(&ui, 2.0);
        dump(&c, "favicon");
        let g = ui.geom();
        let (mx, my) = (g.capsule.x + g.capsule.h / 2.0, g.capsule.y + g.capsule.h / 2.0);
        let at = ((my * 2.0) as usize * c.width + (mx * 2.0) as usize) * 4;
        let px = &c.pixels[at..at + 3];
        assert!(px[0] > 180 && px[1] < 80, "the icon sits in the circle: {px:?}");
        // another host: back to the letter
        ui.tabs[i].icon = Some(("elsewhere.example".into(), ui.tabs[i].icon.take().unwrap().1));
        assert!(ui.tab().favicon().is_none());
    }

    #[test]
    fn background_tab_announces_itself() {
        let mut ui = browsing();
        ui.set_hover(Hit::Page);
        settle(&mut ui);
        assert!(ui.dock_show < 0.01);
        let active = ui.active;
        let load = ui.open_in_background(Url::parse("https://later.example/").unwrap());
        assert_eq!(ui.active, active, "the page you are reading stays");
        for _ in 0..30 {
            ui.tick(0.016);
        }
        assert!(ui.dock_show > 0.9, "the dock comes up to show the new card");
        dump(&frame(&ui, 2.0), "background-tab");
        settle(&mut ui);
        assert!(ui.pulse.is_none() && ui.dock_show < 0.01, "and tucks away again");
        assert!(ui.has_tab(load.tab_id));
    }

    #[test]
    fn dock_tucks_into_a_strip_and_comes_back() {
        let mut ui = browsing();
        let shown = ui.geom().dock;
        ui.set_hover(Hit::Page);
        settle(&mut ui);
        assert!(ui.dock_show < 0.01, "a page is open and the pointer left: the dock hides");
        assert!(ui.geom().dock.y >= ui.h, "tucked below the bottom edge");
        let c = frame(&ui, 2.0);
        dump(&c, "dock-tucked");
        // the strip at the bottom edge brings it back
        let hit = ui.hit(ui.w / 2.0, ui.h - 8.0);
        assert_eq!(hit, Hit::Dock);
        ui.set_hover(hit);
        settle(&mut ui);
        assert!((ui.geom().dock.y - shown.y).abs() < 0.5);
    }

    #[test]
    fn tabs_drag_to_reorder_and_close_one_by_one() {
        let mut ui = browsing();
        let g = ui.geom();
        let ids: Vec<u64> = ui.tabs.iter().map(|t| t.id).collect();
        // a press without moving is a click: it selects
        let x2 = ui.slot_center(&g, 2);
        ui.press_tab(2, x2);
        assert!(ui.release());
        assert_eq!(ui.active, 2);
        // carry tab 0 two slots right: it lands at 2, the others shift left
        ui.select(0);
        let x0 = ui.slot_center(&g, 0);
        ui.press_tab(0, x0);
        for k in 1..=10 {
            ui.drag_to(x0 + k as f32 * theme::SLOT * 0.2);
            ui.tick(0.016);
        }
        assert!(!ui.release(), "a drag is not a click");
        let now: Vec<u64> = ui.tabs.iter().map(|t| t.id).collect();
        assert_eq!(now, vec![ids[1], ids[2], ids[0], ids[3]]);
        assert_eq!(ui.tabs[ui.active].id, ids[0], "the active tab moved with its card");
        // the × on a hovered dot closes just that tab
        settle(&mut ui);
        let x = ui.slot_center(&g, 1);
        ui.set_hover(Hit::Tab(1));
        settle(&mut ui);
        let (bx, by) = close_badge(x, g.dock.y + g.dock.h / 2.0);
        assert_eq!(ui.hit(bx, by), Hit::TabClose(1));
        let c = frame(&ui, 2.0);
        dump(&c, "tab-close");
        ui.close_tab(1);
        let left: Vec<u64> = ui.tabs.iter().map(|t| t.id).collect();
        assert_eq!(left, vec![ids[1], ids[0], ids[3]]);
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
        // the start screen fills the whole window: no frame around it
        assert_eq!(pixel(&c, 2.0, 300.0, 1.0), [theme::PANEL.r, theme::PANEL.g, theme::PANEL.b]);
        assert_eq!(pixel(&c, 500.0, 2.0, 1.0)[0], theme::PANEL.r, "no band above the page");
        assert_eq!(pixel(&c, 30.0, 300.0, 1.0), [theme::PANEL.r, theme::PANEL.g, theme::PANEL.b]);
    }

    #[test]
    fn history_back_forward_and_failures() {
        let mut ui = Ui::new(Transport::Tor, true);
        open(&mut ui, "https://a.example/", "A");
        open(&mut ui, "https://b.example/", "B");
        // without a kept page, going back loads it again
        ui.tabs[ui.active].snapshots.clear();
        let back = ui.back().expect("can go back");
        assert_eq!(back.url.host, "a.example");
        ui.finish(back.tab_id, back.nav, Err("timed out".into()));
        assert!(ui.tab().failure.is_some());
        assert!(ui.active_page().is_none(), "failure screen, not the stale page");
        // B was kept when we left it: forward shows it again at once
        assert!(ui.forward().is_none());
        assert_eq!(ui.active_loaded().unwrap().url.host, "b.example");
        assert!(ui.tab().failure.is_none());
        // a superseded navigation is ignored
        let stale = ui.reload().unwrap();
        let fresh = ui.reload().unwrap();
        ui.finish(stale.tab_id, stale.nav, Err("stale".into()));
        assert!(ui.tab().loading.is_some());
        ui.finish(fresh.tab_id, fresh.nav, Err("fresh".into()));
        assert_eq!(ui.tab().failure.as_ref().unwrap().reason, "fresh");
    }

    #[test]
    fn address_input_searches_when_it_is_not_an_address() {
        let mut ui = Ui::new(Transport::Tor, true);
        ui.begin_editing(false);
        ui.type_text("hello world");
        let load = ui.submit().expect("a search, not a rejection");
        assert_eq!(load.url.host, "lite.duckduckgo.com");
        assert!(load.url.query.contains("q=hello+world"), "query: {}", load.url.query);
        assert!(!ui.editing);
    }

    #[test]
    fn address_input_still_navigates_addresses() {
        let mut ui = Ui::new(Transport::Tor, true);
        ui.begin_editing(false);
        ui.type_text("example.org/path");
        let load = ui.submit().expect("an address");
        assert_eq!(load.url.host, "example.org");
        assert!(!ui.editing);
    }

    #[test]
    fn search_queries_are_encoded() {
        let url = search_url("café & co").expect("search url");
        assert_eq!(url.host, "lite.duckduckgo.com");
        assert!(url.query.contains("caf%C3%A9"));
        assert!(url.query.contains("%26"));
        assert!(search_url("   ").is_none());
    }

    #[test]
    fn bookmarks_round_trip_through_tsv() {
        let original = vec![
            ("Example".to_string(), "https://example.com/".to_string()),
            ("Со ссылкой".to_string(), "https://news.example/a?b=1".to_string()),
        ];
        let text = format_bookmarks(&original);
        assert_eq!(parse_bookmarks(&text), original);
        // comments, blank lines and junk are ignored
        let messy = "# c\n\nhttps://ok.example/\tnope\tnope\nnot a url\n";
        let parsed = parse_bookmarks(messy);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "nope nope");
    }

    #[test]
    fn bookmark_toggle_adds_and_removes() {
        let dir = std::env::temp_dir().join(format!("sepia-bm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SEPIA_BOOKMARKS", dir.join("b.tsv"));
        let mut ui = Ui::new(Transport::Tor, true);
        open(&mut ui, "https://example.com/", "Example Domain");
        assert!(!ui.is_bookmarked());
        assert!(ui.toggle_bookmark());
        assert!(ui.is_bookmarked());
        assert_eq!(ui.bookmarks.len(), 1);
        let saved = std::fs::read_to_string(dir.join("b.tsv")).unwrap();
        assert!(saved.contains("https://example.com/"));
        assert!(!ui.toggle_bookmark());
        assert!(!ui.is_bookmarked());
        std::env::remove_var("SEPIA_BOOKMARKS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_links_download_instead_of_navigating() {
        let file = |u: &str| {
            let url = Url::parse(u).unwrap();
            App::looks_like_file(&url)
        };
        assert!(file("https://example.com/x.zip"));
        assert!(file("https://example.com/deb/pkg.deb"));
        assert!(file("https://example.com/a.PDF"));
        assert!(!file("https://example.com/page.html"));
        assert!(!file("https://example.com/dir/"));
        assert!(!file("https://example.com/no-extension"));
    }

    #[test]
    fn start_page_bookmark_rows_are_clickable() {
        let mut ui = Ui::new(Transport::Tor, true);
        ui.set_size(1100.0, 800.0);
        ui.bookmarks = vec![
            ("Example".into(), "https://example.com/".into()),
            ("Docs".into(), "https://docs.example/".into()),
        ];
        let v = ui.viewport();
        let rows = ui.start_bookmark_rects(&v);
        assert_eq!(rows.len(), 2);
        let row = rows[1];
        let hit = ui.hit(row.x + row.w / 2.0, row.y + row.h / 2.0);
        assert_eq!(hit, Hit::Bookmark(1));
        // and the page area itself is still a page (top-right, away from the dock)
        assert_eq!(ui.hit(v.x + v.w - 12.0, v.y + 80.0), Hit::Page);
    }

    #[test]
    fn download_names_are_safe_and_unique() {
        let url = Url::parse("https://example.com/dir/file.tar.gz").unwrap();
        assert_eq!(download_name(&url), "sepia-file.tar.gz");
        let dir = Url::parse("https://example.com/dir/").unwrap();
        assert_eq!(download_name(&dir), "sepia-dir.html");
        let root = Url::parse("https://example.com/").unwrap();
        assert_eq!(download_name(&root), "sepia-example.com.html");
        let odd = Url::parse("https://example.com/a b?q=1").unwrap();
        assert!(download_name(&odd).starts_with("sepia-"));
        assert!(!download_name(&odd).contains(' '));

        let dir = std::env::temp_dir().join(format!("sepia-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "x.html");
        std::fs::write(&first, b"1").unwrap();
        let second = unique_path(&dir, "x.html");
        assert_ne!(first, second);
        assert_eq!(second.file_name().unwrap(), "x-1.html");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hovered_dot_grows_and_shows_its_title() {
        let mut ui = browsing();
        let g = ui.geom();
        let x = ui.slot_center(&g, 1);
        let cy = g.dock.y + g.dock.h / 2.0;
        let hit = ui.hit(x, cy);
        assert_eq!(hit, Hit::Tab(1));
        ui.set_hover(hit);
        settle(&mut ui);
        assert!((ui.tabs[1].anim.scale - 1.12).abs() < 0.01);
        assert!(ui.tabs[1].anim.hot > 0.99 && ui.tabs[0].anim.hot < 0.01);
        let c = frame(&ui, 2.0);
        dump(&c, "dock-hover");
        // the title plate sits above the island: glass over the white page
        let plate = (8..60).any(|dy| {
            let px = pixel(&c, x, g.dock.y - dy as f32, 2.0);
            px[0] < 250
        });
        assert!(plate, "no title plate above the hovered dot");
        // the three pieces: dots, capsule, + — side by side, none overlapping
        assert!(g.pill.x + g.pill.w < g.capsule.x && g.capsule.x + g.capsule.w < g.new_tab.x);
        assert_eq!(ui.hit(g.new_tab.x + g.new_tab.w / 2.0, cy), Hit::NewTab);
        assert_eq!(ui.hit(g.capsule.x + 40.0, cy), Hit::Capsule);
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
    fn capsule_opens_into_the_address_field() {
        let mut ui = browsing();
        let g = ui.geom();
        assert!(g.capsule.w <= theme::CAPSULE_W + 0.1);
        // hovering alone does not open it; a click (or ⌘L) does
        ui.set_hover(ui.hit(g.capsule.x + 40.0, g.capsule.y + 20.0));
        settle(&mut ui);
        assert!(ui.geom().capsule.w <= theme::CAPSULE_W + 0.1);
        ui.begin_editing(false);
        ui.type_text("news");
        settle(&mut ui);
        let g = ui.geom();
        assert!(g.capsule.w > 600.0);
        // the matching open tab, then what Enter does
        let s = ui.suggest_layout();
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0].kind, Suggest::Tab(1));
        assert_eq!(s.rows[1].kind, Suggest::Go);
        assert!(s.panel.y + s.panel.h <= g.capsule.y);
        let r = s.rows[0].rect;
        assert_eq!(ui.hit(r.x + 30.0, r.y + r.h / 2.0), Hit::Suggest(0));
        ui.set_hover(Hit::Suggest(0));
        dump(&frame(&ui, 2.0), "capsule-open");
        assert!(ui.pick_suggestion(0).is_none());
        assert_eq!(ui.active, 1);
        assert!(!ui.editing);
    }

    #[test]
    fn active_dot_opens_all_tabs() {
        let mut ui = browsing();
        let g = ui.geom();
        let x = ui.slot_center(&g, 0);
        ui.press_tab(0, x);
        assert!(ui.release());
        assert!(ui.overview, "the dot of the tab you are on opens all tabs");
        settle(&mut ui);
        let o = ui.overview_layout();
        assert_eq!(o.cards.len(), ui.tabs.len());
        for r in &o.cards {
            assert!(r.x >= 0.0 && r.x + r.w <= ui.w && r.y + r.h + 44.0 <= o.bar.y, "{r:?}");
        }
        let r = o.cards[2];
        assert_eq!(ui.hit(r.x + r.w / 2.0, r.y + r.h / 2.0), Hit::OverviewTab(2));
        let (bx, by) = overview_close(&r);
        assert_eq!(ui.hit(bx, by), Hit::OverviewClose(2));
        assert_eq!(ui.hit(o.new_tab.x + 10.0, o.new_tab.y + 10.0), Hit::OverviewNew);
        assert_eq!(ui.hit(o.burn.x + 10.0, o.burn.y + 10.0), Hit::OverviewBurn);
        ui.set_hover(Hit::OverviewTab(1));
        dump(&frame(&ui, 2.0), "overview");
        ui.close_overview();
        ui.set_hover(Hit::Dock);
        settle(&mut ui);
        assert_eq!(ui.hit(x, g.dock.y + g.dock.h / 2.0), Hit::Tab(0));
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
        ui.tabs[ui.active].snapshots.clear();
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
        time("cards", &mut || ui.draw_cards(&mut Painter { c: &mut canvas, s }, &fonts, &g, Ink { t: 1.0 }));
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
        assert_eq!(pixel(&c, 2.0, 300.0, 2.0), [255, 255, 255], "the page runs to the window edges");
        assert_eq!(pixel(&c, 100.0, 20.0, 2.0), [255, 255, 255], "the page runs to the top");
    }
}
