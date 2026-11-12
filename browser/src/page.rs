//! One entry point that turns a URL into a laid-out page, whatever the scheme.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::css::Stylesheet;
use crate::dom;
use crate::images::{self, ImageMap};
use crate::js::Session;
use crate::layout::{Fonts, Layout, Page};
use crate::net::{fetch_in, Chain, PageRequest, Response};
use crate::protocols;
use crate::resolver::Resolver;
use crate::url::Url;

pub struct Loaded {
    pub url: Url,
    pub title: String,
    pub page: Page,
    #[allow(dead_code)]
    pub status: u16,
    pub rules: usize,
    pub note: String,
}

/// Fail early with a useful message when a name needs a transport we lack.
fn require_overlay(url: &Url, chain: &Chain) -> Result<(), String> {
    if url.needs_overlay().is_some() && chain.hops.is_empty() {
        return Err(protocols::overlay_hint(url)
            .unwrap_or("this name needs an overlay network")
            .to_string());
    }
    Ok(())
}

pub fn load(
    url: &Url,
    chain: &Chain,
    resolver: Option<&Resolver>,
    width: f32,
    fonts: &Fonts,
    timeout: Duration,
    insecure_tls: bool,
) -> Result<Loaded, String> {
    open(url, chain, resolver, width, fonts, timeout, insecure_tls, false, None).map(|(loaded, _, _)| loaded)
}

/// Images still on their way: the page is shown without them first.
pub type LateImages = std::thread::JoinHandle<ImageMap>;

/// `load`, keeping the page's script session alive for events (clicks,
/// typing). A session is tied to the calling thread. With `defer_images`
/// the page is laid out as soon as its HTML, CSS and scripts are in; the
/// images arrive through the returned handle (give them to the session).
#[allow(clippy::too_many_arguments)]
pub fn open(
    url: &Url,
    chain: &Chain,
    resolver: Option<&Resolver>,
    width: f32,
    fonts: &Fonts,
    timeout: Duration,
    insecure_tls: bool,
    defer_images: bool,
    form: Option<&str>,
) -> Result<(Loaded, Option<Session>, Option<LateImages>), String> {
    let final_site = crate::cookies::site_of(&url.host);
    require_overlay(url, chain)?;
    match url.scheme.as_str() {
        "gemini" => {
            let text_page =
                protocols::fetch_gemini(url, chain, resolver, timeout, insecure_tls)?;
            Ok((finish(url, text_page.html, 20, text_page.title, text_page.note, width, fonts, 0, None, false).0, None, None))
        }
        "gopher" => {
            let text_page = protocols::fetch_gopher(url, chain, resolver, timeout)?;
            Ok((finish(url, text_page.html, 200, text_page.title, text_page.note, width, fonts, 0, None, false).0, None, None))
        }
        _ => {
            // YouTube: videos play in mpv, search runs through yt-dlp; both
            // through the browser's own path (see media.rs)
            if let Some(route) = crate::media::route(url) {
                let (title, html) = crate::media::page(&route, chain, resolver);
                return Ok(finish(
                    url,
                    html,
                    200,
                    title,
                    "youtube via mpv/yt-dlp".into(),
                    width,
                    fonts,
                    0,
                    Some(ImageFetch { chain, resolver, timeout, site: final_site.clone() }),
                    defer_images,
                ));
            }
            let response: Response = fetch_in(url, chain, resolver, timeout, &PageRequest { form, ..PageRequest::get(&final_site) })?;
            let body = String::from_utf8_lossy(&response.body).to_string();
            Ok(finish(
                url,
                body,
                response.status,
                String::new(),
                format!("http {}", response.status),
                width,
                fonts,
                0,
                Some(ImageFetch { chain, resolver, timeout, site: final_site.clone() }),
                defer_images,
            ))
        }
    }
}

/// How to fetch a page's images and stylesheets: the page's own chain and
/// resolver, so these requests leave exactly like the page request did.
struct ImageFetch<'a> {
    chain: &'a Chain,
    resolver: Option<&'a Resolver>,
    timeout: Duration,
    /// the page's site: whose cookie partition subresources use
    site: String,
}

impl ImageFetch<'_> {
    fn get(&self, url: &Url, timeout: Duration) -> Result<Response, String> {
        fetch_in(url, self.chain, self.resolver, timeout, &PageRequest::get(&self.site))
    }
}

const MAX_IMAGES: usize = 64;
const IMAGE_PARALLEL: usize = 6;
/// Images never hold a page back longer than this, whatever the network does.
const IMAGE_BUDGET: Duration = Duration::from_secs(8);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(6);

/// Requests not worth making: 1×1 images, which show nothing and exist to
/// report that the page was viewed.
fn worth_fetching(node: &dom::Node, url: &Url) -> bool {
    let tiny = |name: &str| node.attr(name).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).is_some_and(|v| v <= 1.0);
    let _ = url;
    !(tiny("width") && tiny("height"))
}

/// Decode every `<img src>` of the document. `data:` URIs are decoded in
/// place; http(s) ones go through `fetch_with` (chain + DoH, the fixed header
/// set, no Referer, no cookies), six at a time, within one overall budget.
/// Returns the images and (found, loaded) counts.
fn fetch_images(document: &dom::Node, base: &Url, how: &ImageFetch) -> (ImageMap, usize, usize) {
    fn collect<'n>(node: &'n dom::Node, out: &mut Vec<(&'n str, &'n dom::Node)>) {
        if node.tag == "img" {
            if let Some(src) = node.attr("src").map(str::trim).filter(|s| !s.is_empty()) {
                if !out.iter().any(|(s, _)| *s == src) {
                    out.push((src, node));
                }
            }
        }
        for child in &node.children {
            collect(child, out);
        }
    }
    let mut sources = Vec::new();
    collect(document, &mut sources);
    sources.truncate(MAX_IMAGES);

    let mut images = ImageMap::new();
    let mut remote: Vec<(&str, Url)> = Vec::new();
    for (src, node) in &sources {
        if src.starts_with("data:") {
            if let Some(bitmap) = images::decode_data_uri(src) {
                images.insert(src.to_string(), Arc::new(bitmap));
            }
        } else if let Some(url) = base.resolve(src) {
            if matches!(url.scheme.as_str(), "http" | "https") && worth_fetching(node, &url) {
                remote.push((src, url));
            }
        }
    }
    let started = Instant::now();
    for batch in remote.chunks(IMAGE_PARALLEL) {
        let left = IMAGE_BUDGET.saturating_sub(started.elapsed());
        if left < Duration::from_millis(500) {
            break;
        }
        let per_image = how.timeout.min(IMAGE_TIMEOUT).min(left);
        let decoded: Vec<(&str, Option<images::Bitmap>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(src, url)| {
                    scope.spawn(move || {
                        let bitmap = how.get(url, per_image)
                            .ok()
                            .filter(|r| r.status < 400)
                            .and_then(|r| images::decode(&r.body));
                        (*src, bitmap)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (src, bitmap) in decoded {
            if let Some(bitmap) = bitmap {
                images.insert(src.to_string(), Arc::new(bitmap));
            }
        }
    }
    let loaded = images.len();
    (images, sources.len(), loaded)
}

#[allow(clippy::too_many_arguments)]
fn finish(
    url: &Url,
    html: String,
    status: u16,
    title_hint: String,
    mut note: String,
    width: f32,
    fonts: &Fonts,
    extra_rules: usize,
    fetch: Option<ImageFetch>,
    defer_images: bool,
) -> (Loaded, Option<Session>, Option<LateImages>) {
    let document = dom::parse_document(&html);
    let title = {
        let from_doc = Layout::title(&document);
        if from_doc.is_empty() {
            title_hint
        } else {
            from_doc
        }
    };
    // stylesheets, scripts and images come in parallel; layout needs all three
    // images either come with the page or follow it on their own thread
    let late: Option<LateImages> = match (&fetch, defer_images) {
        (Some(how), true) => {
            let (doc, base, chain, resolver, timeout, site) = (document.clone(), url.clone(), how.chain.clone(), how.resolver.cloned(), how.timeout, how.site.clone());
            Some(std::thread::spawn(move || fetch_images(&doc, &base, &ImageFetch { chain: &chain, resolver: resolver.as_ref(), timeout, site }).0))
        }
        _ => None,
    };
    let (mut sheet, images, scripts) = match &fetch {
        Some(how) => {
            let (sheet, external, scripts, (images, found, loaded)) = std::thread::scope(|scope| {
                let images = (!defer_images).then(|| scope.spawn(|| fetch_images(&document, url, how)));
                let scripts = scope.spawn(|| load_scripts(&document, url, how));
                let (sheet, external) = load_styles(&document, url, Some(how));
                (
                    sheet,
                    external,
                    scripts.join().unwrap_or_default(),
                    images.map(|h| h.join().unwrap_or_else(|_| (ImageMap::new(), 0, 0))).unwrap_or_else(|| (ImageMap::new(), 0, 0)),
                )
            });
            if external > 0 {
                note = format!("{note} · css {external}");
            }
            if !scripts.is_empty() {
                note = format!("{note} · js {}", scripts.len());
            }
            if found > 0 {
                note = format!("{note} · images {loaded}/{found}");
            }
            (sheet, images, scripts)
        }
        None => (load_styles(&document, url, None).0, ImageMap::new(), Vec::new()),
    };
    sheet.viewport_width = width;
    if let Some(how) = &fetch {
        let loaded = load_fonts(&sheet, fonts, how);
        if loaded > 0 {
            note = format!("{note} · fonts {loaded}");
        }
    }
    let rules = sheet.rules.len() + extra_rules;
    if scripts.is_empty() && late.is_none() {
        let page = Layout::new(fonts, &sheet, width).with_images(&images).build(&document);
        return (Loaded { url: url.clone(), title, page, status, rules, note }, None, None);
    }
    // run the page's scripts; the session stays for events
    let net = fetch.as_ref().map(|how| crate::js::NetAccess { chain: how.chain, resolver: how.resolver, timeout: how.timeout });
    let session = Session::start(&document, url, &scripts, net, sheet, images, width);
    let errors = session.log().len();
    if errors > 0 {
        note = format!("{note} · js log {errors}");
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            for line in session.log() {
                eprintln!("js: {line}");
            }
        }
    }
    let page = session.layout(fonts);
    let title = session.title().unwrap_or(title);
    (Loaded { url: url.clone(), title, page, status, rules, note }, Some(session), late)
}

const MAX_FONTS: usize = 16;
const FONT_BUDGET: Duration = Duration::from_secs(6);

/// Fetch the files of the page's `@font-face` rules (TTF, OTF, WOFF; the
/// first usable source of each) through the page's path and register them
/// by family. Returns how many faces were registered.
fn load_fonts(sheet: &Stylesheet, fonts: &Fonts, how: &ImageFetch) -> usize {
    let faces: Vec<&crate::css::FontFace> = sheet.font_faces.iter().take(MAX_FONTS).collect();
    if faces.is_empty() {
        return 0;
    }
    let started = Instant::now();
    let mut registered = 0;
    for batch in faces.chunks(IMAGE_PARALLEL) {
        let left = FONT_BUDGET.saturating_sub(started.elapsed());
        if left < Duration::from_millis(300) {
            break;
        }
        let got: Vec<(&crate::css::FontFace, Option<&'static fontdue::Font>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|face| {
                    scope.spawn(move || {
                        let font = face.sources.iter().find_map(|source| {
                            let url = Url::parse(source)?;
                            let response = how.get(&url, how.timeout.min(left)).ok()?;
                            (response.status < 400).then_some(())?;
                            crate::fonts::load_web_font(source, &response.body)
                        });
                        (*face, font)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (face, font) in got {
            if let Some(font) = font {
                fonts.web.register(&face.family, face.bold, face.italic, font);
                registered += 1;
            }
        }
    }
    registered
}

const MAX_SCRIPTS: usize = 48;
const MAX_SCRIPT_BYTES: usize = 4 * 1024 * 1024;

/// The document's classic scripts in order: inline ones as written, `src`
/// ones fetched through the page's path (any origin, like a stylesheet;
/// six at a time within the CSS budget). Modules, JSON, templates and other
/// non-script types are skipped.
fn load_scripts(document: &dom::Node, base: &Url, how: &ImageFetch) -> crate::js::Scripts {
    enum Source {
        Inline(String),
        Remote(Url),
    }
    fn walk(node: &dom::Node, base: &Url, out: &mut Vec<Source>) {
        if node.tag == "script" {
            let kind = node.attr("type").unwrap_or("").trim().to_ascii_lowercase();
            let classic = kind.is_empty() || kind.contains("javascript") || kind == "text/ecmascript" || kind == "application/ecmascript";
            if classic && node.attr("nomodule").is_none() {
                match node.attr("src") {
                    Some(src) => {
                        if let Some(url) = base.resolve(src).filter(|u| matches!(u.scheme.as_str(), "http" | "https")) {
                            out.push(Source::Remote(url));
                        }
                    }
                    None => out.push(Source::Inline(node.text_content())),
                }
            }
            return;
        }
        for child in &node.children {
            walk(child, base, out);
        }
    }
    let mut sources = Vec::new();
    walk(document, base, &mut sources);
    sources.truncate(MAX_SCRIPTS);
    let remote: Vec<&Url> = sources.iter().filter_map(|s| match s {
        Source::Remote(u) => Some(u),
        _ => None,
    }).collect();
    let started = Instant::now();
    let mut fetched: Vec<(Url, String)> = Vec::new();
    for batch in remote.chunks(IMAGE_PARALLEL) {
        let left = CSS_BUDGET.saturating_sub(started.elapsed());
        if left < Duration::from_millis(300) {
            break;
        }
        let timeout = how.timeout.min(left);
        let got: Vec<(Url, Option<String>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|url| {
                    scope.spawn(move || {
                        let body = how.get(url, timeout)
                            .ok()
                            .filter(|r| r.status < 400 && r.body.len() <= MAX_SCRIPT_BYTES)
                            .map(|r| String::from_utf8_lossy(&r.body).into_owned());
                        ((*url).clone(), body)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        fetched.extend(got.into_iter().filter_map(|(u, b)| b.map(|b| (u, b))));
    }
    sources
        .into_iter()
        .filter_map(|s| match s {
            Source::Inline(code) => (!code.trim().is_empty()).then(|| (code, "inline script".to_string())),
            Source::Remote(url) => fetched.iter().find(|(u, _)| *u == url).map(|(_, code)| (code.clone(), url.to_string())),
        })
        .collect()
}

const MAX_STYLESHEETS: usize = 24;
const MAX_CSS_BYTES: usize = 2 * 1024 * 1024;
/// Stylesheets never hold a page back longer than this.
const CSS_BUDGET: Duration = Duration::from_secs(8);

/// Inline `<style>` only (no network): what tests and text pages use.
#[allow(dead_code)]
pub fn collect_styles(document: &dom::Node, sheet: &mut Stylesheet) {
    if let Some(base) = Url::parse("http://localhost/") {
        sheet.append(load_styles(document, &base, None).0);
    }
}

/// Every stylesheet of the document in cascade order: `<style>` blocks and
/// `<link rel=stylesheet>` files (and their `@import`s) where they appear.
/// Files are fetched through the page's own path when `how` is given, six at
/// a time within `CSS_BUDGET`; print-only sheets are skipped. Returns the
/// combined sheet and how many external files were loaded.
fn load_styles(document: &dom::Node, base: &Url, how: Option<&ImageFetch>) -> (Stylesheet, usize) {
    enum Source {
        Inline(String),
        Link(Url),
    }
    fn print_only(media: Option<&str>) -> bool {
        media.is_some_and(|m| {
            let m = m.to_ascii_lowercase();
            m.contains("print") && !m.contains("screen") && !m.contains("all")
        })
    }
    fn walk(node: &dom::Node, base: &Url, out: &mut Vec<Source>) {
        match node.tag.as_str() {
            "style" if !print_only(node.attr("media")) => out.push(Source::Inline(node.text_content())),
            "link" => {
                let rel = node.attr("rel").unwrap_or("").to_ascii_lowercase();
                let is_sheet = rel.split_whitespace().any(|r| r == "stylesheet") && !rel.contains("alternate");
                if is_sheet && !print_only(node.attr("media")) {
                    if let Some(url) = node.attr("href").and_then(|h| base.resolve(h)) {
                        if matches!(url.scheme.as_str(), "http" | "https") {
                            out.push(Source::Link(url));
                        }
                    }
                }
            }
            _ => {}
        }
        for child in &node.children {
            walk(child, base, out);
        }
    }
    let mut sources = Vec::new();
    walk(document, base, &mut sources);

    // fetch every linked file up front, in parallel
    let links: Vec<&Url> = sources
        .iter()
        .filter_map(|s| match s {
            Source::Link(u) => Some(u),
            _ => None,
        })
        .take(MAX_STYLESHEETS)
        .collect();
    let started = Instant::now();
    let mut fetched: Vec<(Url, String)> = Vec::new();
    if let Some(how) = how {
        for batch in links.chunks(IMAGE_PARALLEL) {
            let left = CSS_BUDGET.saturating_sub(started.elapsed());
            if left < Duration::from_millis(300) {
                break;
            }
            let timeout = how.timeout.min(left);
            let got: Vec<(Url, Option<String>)> = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|url| scope.spawn(move || ((*url).clone(), fetch_css(url, how, timeout))))
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            fetched.extend(got.into_iter().filter_map(|(u, css)| css.map(|c| (u, c))));
        }
    }

    let mut sheet = Stylesheet::default();
    let mut external = 0;
    let mut imports_left = 8;
    for source in &sources {
        let (text, sheet_url) = match source {
            Source::Inline(text) => (text.clone(), base.clone()),
            Source::Link(url) => match fetched.iter().find(|(u, _)| u == url) {
                Some((_, css)) => {
                    external += 1;
                    (css.clone(), url.clone())
                }
                None => continue,
            },
        };
        let parsed = Stylesheet::parse(&text);
        // @import comes before the rules of the sheet that imports it
        if let Some(how) = how {
            for import in &parsed.imports {
                if imports_left == 0 || started.elapsed() > CSS_BUDGET {
                    break;
                }
                imports_left -= 1;
                let Some(url) = sheet_url.resolve(import) else { continue };
                let left = CSS_BUDGET.saturating_sub(started.elapsed());
                if let Some(css) = fetch_css(&url, how, how.timeout.min(left)) {
                    external += 1;
                    let mut imported = Stylesheet::parse(&css);
                    imported.imports.clear();
                    sheet.append(imported);
                }
            }
        }
        let mut parsed = parsed;
        parsed.imports.clear();
        for face in &mut parsed.font_faces {
            face.sources = face.sources.iter().filter_map(|s| sheet_url.resolve(s).map(|u| u.to_string())).collect();
        }
        sheet.append(parsed);
    }
    (sheet, external)
}

fn fetch_css(url: &Url, how: &ImageFetch, timeout: Duration) -> Option<String> {
    let response = how.get(url, timeout).ok()?;
    if response.status >= 400 || response.body.len() > MAX_CSS_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&response.body).into_owned())
}
