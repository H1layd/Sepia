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
    /// Number of script elements in the fetched document.
    #[allow(dead_code)]
    pub scripts: usize,
    #[allow(dead_code)]
    pub status: u16,
    pub rules: usize,
    pub note: String,
    /// The site's icon (`<link rel=icon>`, else `/favicon.ico`), for the tab.
    pub icon: Option<std::sync::Arc<crate::images::Bitmap>>,
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

// kept for the public API (HANDOFF); the CLI and the window use `open`
#[allow(dead_code)]
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
    let started = Instant::now();
    let mut current = url.clone();
    let mut body = form.map(str::to_string);
    let mut visited = Vec::new();
    for _ in 0..=8 {
        let key = (current.to_string(), body.clone());
        // a page may reload itself once (it set a cookie, say); twice is a loop
        if visited.iter().filter(|k| **k == key).count() >= 2 {
            return Err("script navigation loop".into());
        }
        if std::env::var_os("SEPIA_VERBOSE").is_some() && !visited.is_empty() {
            eprintln!("script navigation: {}", key.0);
        }
        visited.push(key);
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() { return Err("script navigation timed out".into()); }
        let (loaded, mut session, late) = open_once(&current, chain, resolver, width, fonts, remaining, insecure_tls, defer_images, body.as_deref())?;
        let navigation = session.as_mut().and_then(Session::take_startup_navigation);
        let Some((target, form)) = navigation else { return Ok((loaded, session, late)); };
        // Fragments are handled by the document, not a second network load.
        if target.starts_with('#') { return Ok((loaded, session, late)); }
        current = current.resolve(&target).ok_or_else(|| "unsupported script navigation URL".to_string())?;
        body = form;
        // Images requested by the old document may finish on their own, but
        // can no longer update the new page's session or GUI generation.
        drop(late);
    }
    Err("too many script navigations (limit 8)".into())
}

#[allow(clippy::too_many_arguments)]
fn open_once(
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
            let html_started = Instant::now();
            let response: Response = fetch_in(url, chain, resolver, timeout, &PageRequest { form, ..PageRequest::get(&final_site) })?;
            timing("html", html_started);
            let kind = response.content_type();
            let html = kind.is_empty() || kind.contains("html") || kind.contains("xml");
            let (body, encoding) = crate::charset::decode(&response.body, &kind, html, crate::charset::Encoding::Utf8);
            let opened = finish(
                url,
                body,
                response.status,
                String::new(),
                format!("http {}", response.status),
                width,
                fonts,
                0,
                Some(ImageFetch { page: Some(url.clone()), encoding, chain, resolver, timeout, site: final_site.clone() }),
                defer_images,
            );
            // an error answer that shows nothing (a bot wall, a rate limit, a
            // challenge only a script engine passes): say what happened instead
            // of a blank page
            let shown: usize = opened.0.page.text.iter().map(|t| t.trim().chars().count()).sum();
            if response.status >= 400 && shown < 40 {
                let html = blocked_page(url, &response);
                let (mut loaded, _, _) = finish(url, html, response.status, String::new(), format!("http {} · explained", response.status), width, fonts, 0, None, false);
                loaded.status = response.status;
                return Ok((loaded, None, None));
            }
            Ok(opened)
        }
    }
}

/// How to fetch a page's images and stylesheets: the page's own chain and
/// resolver, so these requests leave exactly like the page request did.
struct ImageFetch<'a> {
    /// the page asking: its address is the Referer of what it loads
    page: Option<Url>,
    /// the document's encoding: scripts and stylesheets that name none use it
    encoding: crate::charset::Encoding,
    chain: &'a Chain,
    resolver: Option<&'a Resolver>,
    timeout: Duration,
    /// the page's site: whose cookie partition subresources use
    site: String,
}

impl ImageFetch<'_> {
    fn get(&self, url: &Url, timeout: Duration) -> Result<Response, String> {
        fetch_in(url, self.chain, self.resolver, timeout, &PageRequest { from: self.page.as_ref(), ..PageRequest::get(&self.site) })
    }
}

const MAX_IMAGES: usize = usize::MAX;
const IMAGE_PARALLEL: usize = 6;
/// Images never hold a page back longer than this, whatever the network does.
const IMAGE_BUDGET: Duration = Duration::from_secs(365 * 24 * 3600);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(6);

/// Requests not worth making: 1×1 images, which show nothing and exist to
/// report that the page was viewed.
fn worth_fetching(_node: &dom::Node, _url: &Url) -> bool {
    // every image is requested, as in any browser (1x1 pixels included)
    true
}

/// Decode every `<img src>` of the document. `data:` URIs are decoded in
/// place; http(s) ones go through `fetch_with` (chain + DoH, the fixed header
/// set, no Referer, no cookies), six at a time, within one overall budget.
/// Returns the images and (found, loaded) counts.
fn fetch_images(document: &dom::Node, base: &Url, how: &ImageFetch) -> (ImageMap, usize, usize) {
    fn collect<'n>(node: &'n dom::Node, out: &mut Vec<(&'n str, &'n dom::Node)>) {
        if matches!(node.tag.as_str(), "template" | "noscript") { return; }
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

/// Fetch and decode images named by URL strings (resolved against `base`;
/// `data:` decoded in place), keyed by the string as given.
fn fetch_list(wanted: &[String], base: &Url, how: &ImageFetch) -> ImageMap {
    let mut images = ImageMap::new();
    let mut remote: Vec<(&str, Url)> = Vec::new();
    for src in wanted {
        if src.starts_with("data:") {
            if let Some(bitmap) = images::decode_data_uri(src) {
                images.insert(src.clone(), Arc::new(bitmap));
            }
        } else if let Some(u) = base.resolve(src).filter(|u| matches!(u.scheme.as_str(), "http" | "https")) {
            remote.push((src, u));
        }
    }
    let started = Instant::now();
    for batch in remote.chunks(IMAGE_PARALLEL) {
        let left = IMAGE_BUDGET.saturating_sub(started.elapsed());
        if left < Duration::from_millis(500) {
            break;
        }
        let per_image = how.timeout.min(IMAGE_TIMEOUT).min(left);
        let got: Vec<(&str, Option<images::Bitmap>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(src, u)| scope.spawn(move || (*src, how.get(u, per_image).ok().filter(|r| r.status < 400).and_then(|r| images::decode(&r.body)))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (src, bitmap) in got {
            if let Some(bitmap) = bitmap {
                images.insert(src.to_string(), Arc::new(bitmap));
            }
        }
    }
    images
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
    let document = dom::complete_document(dom::parse_document(&html));
    let script_count = count_scripts(&document);
    let mut icon = None;
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
            Some(std::thread::spawn(move || fetch_images(&doc, &base, &ImageFetch { page: Some(base.clone()), encoding: crate::charset::Encoding::Utf8, chain: &chain, resolver: resolver.as_ref(), timeout, site }).0))
        }
        _ => None,
    };
    let subresources = Instant::now();
    let (mut sheet, images, scripts) = match &fetch {
        Some(how) => {
            let (sheet, external, scripts, (images, found, loaded)) = std::thread::scope(|scope| {
                let images = (!defer_images).then(|| scope.spawn(|| fetch_images(&document, url, how)));
                let scripts = scope.spawn(|| load_scripts(&document, url, how));
                let favicon = scope.spawn(|| fetch_icon(&document, url, how));
                let (sheet, external) = load_styles(&document, url, Some(how));
                icon = favicon.join().ok().flatten();
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
    // background images: known only now that the styles are in
    let mut images = images;
    let mut late = late;
    if let Some(how) = &fetch {
        let wanted: Vec<String> = sheet.background_urls(&document).into_iter().filter(|u| !images.contains_key(u)).collect();
        if !wanted.is_empty() {
            note = format!("{note} · backgrounds {}", wanted.len());
            match late.take() {
                // the page shows first; backgrounds follow with the images
                Some(first) => {
                    let (base, chain, resolver, timeout, site) = (url.clone(), how.chain.clone(), how.resolver.cloned(), how.timeout, how.site.clone());
                    late = Some(std::thread::spawn(move || {
                        let mut map = first.join().unwrap_or_default();
                        map.extend(fetch_list(&wanted, &base, &ImageFetch { page: Some(base.clone()), encoding: crate::charset::Encoding::Utf8, chain: &chain, resolver: resolver.as_ref(), timeout, site }));
                        map
                    }));
                }
                None => images.extend(fetch_list(&wanted, url, how)),
            }
        }
    }
    if let Some(how) = &fetch {
        timing("styles, scripts, images", subresources);
        let fonts_started = Instant::now();
        let loaded = load_fonts(&sheet, fonts, how, &document.text_content());
        timing("fonts", fonts_started);
        if loaded > 0 {
            note = format!("{note} · fonts {loaded}");
        }
    }
    let rules = sheet.rules.len() + extra_rules;
    // a session even without scripts: buttons, forms and fields need it
    if fetch.is_none() && scripts.is_empty() && late.is_none() {
        let page = Layout::new(fonts, &sheet, width).with_images(&images).build(&document);
        return (Loaded { url: url.clone(), title, page, scripts: script_count, status, rules, note, icon }, None, None);
    }
    // run the page's scripts; the session stays for events
    let net = fetch.as_ref().map(|how| crate::js::NetAccess { chain: how.chain, resolver: how.resolver, timeout: how.timeout });
    let script_started = Instant::now();
    let mut session = Session::start(&document, url, &scripts, net, sheet, images, width);
    session.debug_eval();
    if std::env::var_os("SEPIA_VERBOSE").is_some() {
        eprintln!("page timing: script execution {:.2} ms ({})", script_started.elapsed().as_secs_f64() * 1000.0, url.to_string());
    }
    let errors = session.log().len();
    if errors > 0 {
        note = format!("{note} · js log {errors}");
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            for line in session.log() {
                eprintln!("js: {line}");
            }
        }
    }
    // SEPIA_LAYOUT_REPEAT=n: lay the page out n more times (profiling)
    if let Some(n) = std::env::var("SEPIA_LAYOUT_REPEAT").ok().and_then(|v| v.parse::<usize>().ok()) {
        let again = Instant::now();
        for _ in 0..n {
            let _ = session.layout(fonts);
        }
        eprintln!("page timing: {n} more layouts {} ms", again.elapsed().as_millis());
    }
    let layout_started = Instant::now();
    let page = session.layout(fonts);
    if std::env::var_os("SEPIA_VERBOSE").is_some() {
        eprintln!("page timing: layout {:.2} ms ({})", layout_started.elapsed().as_secs_f64() * 1000.0, url.to_string());
    }
    let title = session.title().unwrap_or(title);
    (Loaded { url: url.clone(), title, page, scripts: script_count, status, rules, note, icon }, Some(session), late)
}

const ICON_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_ICON_BYTES: usize = usize::MAX;

/// The page's icon: the best `<link rel~=icon>` (an exact 32px size or SVG
/// first, `apple-touch-icon` last), else `/favicon.ico`; fetched like an
/// image (the page's path, its site's cookies), `data:` decoded in place.
fn fetch_icon(document: &dom::Node, base: &Url, how: &ImageFetch) -> Option<std::sync::Arc<crate::images::Bitmap>> {
    fn walk<'a>(n: &'a dom::Node, out: &mut Vec<&'a dom::Node>) {
        if matches!(n.tag.as_str(), "template" | "noscript" | "body") {
            return;
        }
        if n.tag == "link" {
            let rel = n.attr("rel").unwrap_or("").to_ascii_lowercase();
            if rel.split_whitespace().any(|r| r == "icon" || r == "apple-touch-icon") && n.attr("href").is_some_and(|h| !h.trim().is_empty()) {
                out.push(n);
            }
        }
        for c in &n.children {
            walk(c, out);
        }
    }
    let mut links = Vec::new();
    walk(document, &mut links);
    let rank = |n: &&dom::Node| {
        let rel = n.attr("rel").unwrap_or("").to_ascii_lowercase();
        let sizes = n.attr("sizes").unwrap_or("").to_ascii_lowercase();
        let svg = n.attr("type").is_some_and(|t| t.contains("svg")) || n.attr("href").is_some_and(|h| h.ends_with(".svg"));
        let px = sizes.split('x').next().and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(0);
        let fit = if svg || sizes == "any" { 0 } else if px == 0 { 2 } else { 1 + (px - 32).abs() / 16 };
        (rel.contains("apple"), fit)
    };
    links.sort_by_key(rank);
    let mut candidates: Vec<String> = links.iter().filter_map(|n| n.attr("href").map(|h| h.trim().to_string())).collect();
    candidates.push("/favicon.ico".into());
    for href in candidates.into_iter().take(3) {
        if href.starts_with("data:") {
            if let Some(b) = crate::images::decode_data_uri(&href) {
                return Some(std::sync::Arc::new(b));
            }
            continue;
        }
        let Some(url) = base.resolve(&href).filter(|u| matches!(u.scheme.as_str(), "http" | "https")) else { continue };
        let response = match how.get(&url, how.timeout.min(ICON_TIMEOUT)) {
            Ok(r) => r,
            Err(e) => {
                if std::env::var_os("SEPIA_VERBOSE").is_some() { eprintln!("icon not loaded: {}: {e}", url.to_string()); }
                continue;
            }
        };
        if response.status >= 400 || response.body.len() > MAX_ICON_BYTES {
            if std::env::var_os("SEPIA_VERBOSE").is_some() { eprintln!("icon not loaded: {}: HTTP {}, {} bytes", url.to_string(), response.status, response.body.len()); }
            continue;
        }
        if let Some(b) = crate::images::decode(&response.body) {
            return Some(std::sync::Arc::new(b));
        }
    }
    None
}

/// --verbose: how long a phase of loading took
fn timing(phase: &str, since: Instant) {
    if std::env::var_os("SEPIA_VERBOSE").is_some() {
        eprintln!("page timing: {phase} {} ms", since.elapsed().as_millis());
    }
}

const MAX_FONTS: usize = usize::MAX;
const FONT_BUDGET: Duration = Duration::from_secs(365 * 24 * 3600);

/// Fetch the files of the page's `@font-face` rules (TTF, OTF, WOFF; the
/// first usable source of each) through the page's path and register them
/// by family. Returns how many faces were registered.
fn load_fonts(sheet: &Stylesheet, fonts: &Fonts, how: &ImageFetch, text: &str) -> usize {
    // only the subsets the page's text needs (scripts write more later: the
    // basic Latin and the user's own language are always wanted)
    let mut chars: std::collections::HashSet<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    chars.extend((0x20u8..0x7f).map(char::from));
    chars.extend("абвгдеёжзийклмнопрстуфхцчшщъыьэюяàèéìòùç".chars());
    let mut seen = std::collections::HashSet::new();
    // Latin/Cyrillic subsets first: a page may list greek, vietnamese… too
    let mut faces: Vec<&crate::css::FontFace> = sheet
        .font_faces
        .iter()
        .filter(|f| f.wanted(&chars))
        .filter(|f| seen.insert((f.family.clone(), f.bold, f.italic, f.sources.first().cloned())))
        .collect();
    faces.sort_by_key(|f| !f.common);
    faces.truncate(MAX_FONTS);
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

// browsers run them all; this only stops pathological pages (bytes are capped too)
const MAX_SCRIPTS: usize = usize::MAX;
const MAX_SCRIPT_BYTES: usize = usize::MAX;
const MAX_TOTAL_SCRIPT_BYTES: usize = usize::MAX;

fn bounded_scripts(scripts: impl Iterator<Item = (String, String)>, per_script: usize, total: usize) -> crate::js::Scripts {
    let mut remaining = total;
    scripts.filter(|(code, label)| {
        if code.len() > per_script || code.len() > remaining {
            if std::env::var_os("SEPIA_VERBOSE").is_some() {
                eprintln!("script not loaded: {label}: {} bytes exceeds script/page budget", code.len());
            }
            false
        } else { remaining -= code.len(); true }
    }).collect()
}

/// The document's scripts: classic ones in order (inline as written, `src`
/// fetched through the page's path, any origin like a stylesheet, six at a
/// time within the CSS budget), then modules (labelled `js::MODULE` + URL;
/// their imports load in `js.rs` the same way). JSON, templates, `nomodule`
/// fallbacks and other types are skipped.
fn load_scripts(document: &dom::Node, base: &Url, how: &ImageFetch) -> crate::js::Scripts {
    enum Source {
        Inline(String, bool),
        Remote(Url, bool),
        ImportMap(String),
    }
    fn walk(node: &dom::Node, base: &Url, out: &mut Vec<Source>) {
        if matches!(node.tag.as_str(), "template" | "noscript") { return; }
        if node.tag == "script" {
            let kind = node.attr("type").unwrap_or("").trim().to_ascii_lowercase();
            let classic = kind.is_empty() || kind.contains("javascript") || kind == "text/ecmascript" || kind == "application/ecmascript";
            let module = kind == "module";
            if kind == "importmap" && node.attr("src").is_none() {
                out.push(Source::ImportMap(node.text_content()));
                return;
            }
            // modules run, so their `nomodule` fallbacks must not
            if (classic && node.attr("nomodule").is_none()) || module {
                match node.attr("src") {
                    // a script written into a data: URL runs like an inline one
                    Some(src) if src.trim_start().starts_with("data:") => {
                        if let Some((_, bytes)) = crate::images::data_url_bytes(src) {
                            out.push(Source::Inline(String::from_utf8_lossy(&bytes).into_owned(), module));
                        }
                    }
                    Some(src) => {
                        if let Some(url) = base.resolve(src).filter(|u| matches!(u.scheme.as_str(), "http" | "https")) {
                            out.push(Source::Remote(url, module));
                        }
                    }
                    None => out.push(Source::Inline(node.text_content(), module)),
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
    // modules are deferred: they run after the classic scripts, in order
    sources.sort_by_key(|s| match s {
        Source::ImportMap(_) => 0,
        Source::Inline(_, false) | Source::Remote(_, false) => 1,
        _ => 2,
    });
    sources.truncate(MAX_SCRIPTS);
    let remote: Vec<&Url> = sources.iter().filter_map(|s| match s {
        Source::Remote(u, _) => Some(u),
        _ => None,
    }).collect();
    let started = Instant::now();
    let mut fetched: Vec<(Url, String)> = Vec::new();
    for batch in remote.chunks(SCRIPT_PARALLEL) {
        let left = SCRIPT_BUDGET.saturating_sub(started.elapsed());
        if left < Duration::from_millis(300) {
            break;
        }
        let timeout = how.timeout.min(left);
        let got: Vec<(Url, Option<String>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|url| {
                    scope.spawn(move || {
                        let body = match how.get(url, timeout) {
                            Ok(response) if response.status < 400 && response.body.len() <= MAX_SCRIPT_BYTES =>
                                Some(crate::charset::decode(&response.body, &response.content_type(), false, how.encoding).0),
                            Ok(response) => {
                                if std::env::var_os("SEPIA_VERBOSE").is_some() {
                                    eprintln!("script not loaded: {}: HTTP {}, {} bytes (limit {})", url.to_string(), response.status, response.body.len(), MAX_SCRIPT_BYTES);
                                }
                                None
                            }
                            Err(error) => {
                                if std::env::var_os("SEPIA_VERBOSE").is_some() { eprintln!("script not loaded: {}: {error}", url.to_string()); }
                                None
                            }
                        };
                        ((*url).clone(), body)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        fetched.extend(got.into_iter().filter_map(|(u, b)| b.map(|b| (u, b))));
    }
    let mut inline = 0;
    let mut scripts = bounded_scripts(sources
        .into_iter()
        .filter_map(|s| match s {
            Source::Inline(code, module) => (!code.trim().is_empty()).then(|| (code, { inline += 1; if module { format!("{}{}#inline-module-{inline}", crate::js::MODULE, base.to_string().split('#').next().unwrap_or_default()) } else { format!("inline script {inline}") } })),
            Source::ImportMap(map) => Some((map, format!("{}{}", crate::js::IMPORT_MAP, base.to_string()))),
            Source::Remote(url, module) => fetched.iter().find(|(u, _)| *u == url).map(|(_, code)| (code.clone(), format!("{}{}", if module { crate::js::MODULE } else { "" }, url.to_string()))),
        }), MAX_SCRIPT_BYTES, MAX_TOTAL_SCRIPT_BYTES);
    module_graph(&mut scripts, base, how, started);
    scripts
}

/// The static import graph of the page's modules, fetched here with the rest
/// of the page (12 files at a time) instead of one by one once scripts run.
fn module_graph(scripts: &mut crate::js::Scripts, base: &Url, how: &ImageFetch, started: Instant) {
    let imports: Vec<(String, String)> = scripts
        .iter()
        .filter(|(_, l)| l.starts_with(crate::js::IMPORT_MAP))
        .flat_map(|(code, _)| crate::js::import_map_entries(code, base))
        .collect();
    let mut asked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut frontier: Vec<Url> = Vec::new();
    let mut queue = |from: &Url, code: &str, frontier: &mut Vec<Url>| {
        for spec in crate::js::static_imports(code) {
            if let Ok(u) = crate::js::locate_specifier(&imports, &spec, from) {
                if asked.insert(u.to_string()) {
                    frontier.push(u);
                }
            }
        }
    };
    for (code, label) in scripts.iter() {
        if let Some(from) = label.strip_prefix(crate::js::MODULE).and_then(Url::parse) {
            queue(&from, code, &mut frontier);
        }
    }
    let mut got_bytes = 0usize;
    let mut found = Vec::new();
    while !frontier.is_empty() && got_bytes < MAX_TOTAL_SCRIPT_BYTES {
        let left = SCRIPT_BUDGET.saturating_sub(started.elapsed());
        if left < Duration::from_millis(300) {
            break;
        }
        let batch: Vec<Url> = frontier.drain(..frontier.len().min(SCRIPT_PARALLEL)).collect();
        let got: Vec<(Url, Option<String>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|url| {
                    scope.spawn(move || {
                        let body = how.get(url, how.timeout.min(left)).ok().filter(|r| r.status < 400 && r.body.len() <= MAX_SCRIPT_BYTES)
                            .map(|r| crate::charset::decode(&r.body, &r.content_type(), false, crate::charset::Encoding::Utf8).0);
                        (url.clone(), body)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (url, body) in got {
            if let Some(code) = body {
                got_bytes += code.len();
                queue(&url, &code, &mut frontier);
                found.push((code, format!("{}{}", crate::js::PREFETCHED, url.to_string())));
            }
        }
    }
    if std::env::var_os("SEPIA_VERBOSE").is_some() && !found.is_empty() {
        eprintln!("page timing: module graph with the page {} files", found.len());
    }
    scripts.extend(found);
}

const MAX_STYLESHEETS: usize = usize::MAX;
const MAX_CSS_BYTES: usize = usize::MAX;
/// Stylesheets never hold a page back longer than this.
const CSS_BUDGET: Duration = Duration::from_secs(365 * 24 * 3600);
/// A page without its scripts is broken, so they get longer than styles,
/// and more of them at once (a modern page lists dozens).
const SCRIPT_BUDGET: Duration = Duration::from_secs(365 * 24 * 3600);
const SCRIPT_PARALLEL: usize = 12;

/// Inline `<style>` only (no network): what tests and text pages use.
#[allow(dead_code)]
fn count_scripts(node: &dom::Node) -> usize {
    if matches!(node.tag.as_str(), "template" | "noscript") { return 0; }
    let mut total = usize::from(node.tag == "script");
    for child in &node.children {
        total += count_scripts(child);
    }
    total
}

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
        if matches!(node.tag.as_str(), "template" | "noscript") { return; }
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
    let mut imports_left = usize::MAX;
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
                    imported.absolutize_urls(&url);
                    sheet.append(imported);
                }
            }
        }
        let mut parsed = parsed;
        parsed.imports.clear();
        parsed.absolutize_urls(&sheet_url);
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
    // (an @charset rule would come next; pages mostly say it in the header)
    Some(crate::charset::decode(&response.body, &response.content_type(), false, how.encoding).0)
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn modules_run_last_and_nomodule_fallbacks_not_at_all() {
        let document = dom::parse_document("<script type=importmap>{\"imports\":{}}</script><script type=module>moduleOnly()</script><script nomodule>classicFallback()</script><script>classic()</script>");
        let chain = Chain::default();
        let how = ImageFetch { page: None, encoding: crate::charset::Encoding::Utf8, chain: &chain, resolver: None, timeout: Duration::from_secs(1), site: "example.test".into() };
        let scripts = load_scripts(&document, &Url::parse("https://example.test/").unwrap(), &how);
        let codes: Vec<&str> = scripts.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, ["{\"imports\":{}}", "classic()", "moduleOnly()"]);
        assert!(scripts[0].1.starts_with(crate::js::IMPORT_MAP));
        assert!(scripts[2].1.starts_with(crate::js::MODULE));
    }

    #[test]
    fn script_budgets_keep_whole_sources_in_order() {
        let scripts = vec![("first()".into(), "one".into()), ("tooLargeToRun()".into(), "oversized".into()),
            ("second()".into(), "two".into()), ("third()".into(), "over-page-limit".into())];
        let got = bounded_scripts(scripts.into_iter(), 8, 15);
        assert_eq!(got, vec![("first()".into(), "one".into()), ("second()".into(), "two".into())]);
    }

    #[test]
    fn inert_template_and_noscript_resources_do_not_affect_active_page() {
        let doc = dom::parse_document("<style>p { color: red }</style>\
            <noscript><style>body { display: none }</style><script>bad()</script></noscript>\
            <template><style>p { display: none }</style><script>bad()</script></template>\
            <p>Visible</p><script>active()</script>");
        let url = Url::parse("https://example.test/").unwrap();
        let (sheet, loaded) = load_styles(&doc, &url, None);
        assert_eq!(loaded, 0);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(count_scripts(&doc), 1);
        let page = Layout::new(&Fonts::load(), &sheet, 800.0).build(&doc);
        assert!(page.text.join(" ").contains("Visible"));
    }

    #[test]
    fn script_failures_remain_measurable_in_loaded_note() {
        let url = Url::parse("https://example.test/").unwrap();
        let chain = Chain::default();
        let (loaded, session, _) = finish(
            &url, "<script>throw new Error('diagnostic-regression')</script><p>Content</p>".into(),
            200, String::new(), "http 200".into(), 800.0, &Fonts::load(), 0,
            Some(ImageFetch { page: None, encoding: crate::charset::Encoding::Utf8, chain: &chain, resolver: None, timeout: Duration::from_secs(1), site: "example.test".into() }),
            false,
        );
        let session = session.unwrap();
        let logs = session.log();
        assert!(logs.iter().any(|line| line.contains("diagnostic-regression")));
        assert!(loaded.note.contains(&format!("js log {}", logs.len())));
    }

    #[test]
    fn youtube_navigation_fetches_origin_page_and_keeps_session() {
        // A local HTTP proxy makes the destination observable without contacting
        // the public site or invoking any media tools.
        for destination in [
            "http://www.youtube.com/watch?v=regression",
            "http://www.youtube.com/results?search_query=regression",
            "http://youtu.be/regression",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let chain = Chain::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "navigation never reached HTTP proxy");
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("proxy accept: {error}"),
                    }
                };
                // macOS can inherit the listener's nonblocking mode.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 16_384);
                }
                let body = "<title>Origin response</title><h1>Actual page content</h1>";
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                String::from_utf8(request).unwrap()
            });
            let url = Url::parse(destination).unwrap();
            let (loaded, session, late) = open(
                &url, &chain, None, 800.0, &Fonts::load(),
                Duration::from_secs(30), false, false, None,
            ).unwrap();
            assert!(server.join().unwrap().starts_with(&format!("GET {destination} HTTP/1.1\r\n")));
            assert_eq!(loaded.title, "Origin response");
            assert!(loaded.page.text.join(" ").split_whitespace().collect::<Vec<_>>().join(" ").contains("Actual page content"));
            assert_eq!(loaded.note, "http 200");
            assert!(session.is_some(), "own-engine session must survive navigation");
            assert!(late.is_none());
        }
    }
}


/// The page shown for an error answer that renders nothing: why, and a way to try again.
fn blocked_page(url: &Url, response: &Response) -> String {
    let ru = crate::persona::current().country.language.starts_with("ru");
    let header = |name: &str| response.header(name).unwrap_or("").to_ascii_lowercase();
    let cloudflare = header("server").contains("cloudflare") || response.header("cf-ray").is_some() || response.header("cf-mitigated").is_some();
    let status = response.status;
    let (title, reason) = match (status, cloudflare, ru) {
        (429, _, true) => ("Слишком много запросов", "Сайт ограничил частоту обращений с этого адреса. Подождите немного и повторите."),
        (429, _, false) => ("Too many requests", "The site limits how often this address may ask. Wait a little and try again."),
        (401, _, true) => ("Нужен вход", "Сайт не показывает эту страницу без входа или считает запрос автоматическим."),
        (401, _, false) => ("Sign-in required", "The site does not show this page without signing in, or took the request for a bot."),
        (403 | 498 | 503, true, true) | (403 | 498, false, true) => ("Сайт закрылся от автоматики", "Защита сайта (часто Cloudflare или похожая) не пропустила запрос: она проверяет браузер скриптами или признаками, которые наш движок пока не проходит."),
        (403 | 498 | 503, true, false) | (403 | 498, false, false) => ("The site blocked automated access", "The site's protection (often Cloudflare or similar) did not let the request through: it checks the browser with scripts or signals our engine does not pass yet."),
        (_, _, true) => ("Сайт ответил ошибкой", "Сервер вернул ошибку и не показал содержимого."),
        (_, _, false) => ("The site answered with an error", "The server returned an error and showed nothing."),
    };
    let retry = if ru { "Повторить" } else { "Try again" };
    let detail = if cloudflare { " · Cloudflare" } else { "" };
    let href = url.to_string().replace('&', "&amp;").replace('"', "&quot;");
    let host = url.host.replace('&', "&amp;").replace('<', "&lt;");
    format!(
        "<!doctype html><html><head><title>{title}</title><style>\
         body{{font:16px/1.5 sans-serif;max-width:34em;margin:18vh auto 0;padding:0 2em;color:#222}}\
         h1{{font-size:24px;font-weight:600;margin:0 0 .6em}} p{{margin:0 0 1em}} .code{{color:#777;font-size:14px}}\
         a.retry{{display:inline-block;padding:.5em 1.4em;border-radius:6px;background:#222;color:#fff;text-decoration:none}}\
         </style></head><body><h1>{title}</h1><p>{reason}</p>\
         <p class=code>{host} · HTTP {status}{detail}</p><p><a class=retry href=\"{href}\">{retry}</a></p></body></html>"
    )
}
