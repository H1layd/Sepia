//! One entry point that turns a URL into a laid-out page, whatever the scheme.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::css::Stylesheet;
use crate::dom;
use crate::images::{self, ImageMap};
use crate::layout::{Fonts, Layout, Page};
use crate::net::{fetch_with, Chain, Response};
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
    require_overlay(url, chain)?;
    match url.scheme.as_str() {
        "gemini" => {
            let text_page =
                protocols::fetch_gemini(url, chain, resolver, timeout, insecure_tls)?;
            Ok(finish(url, text_page.html, 20, text_page.title, text_page.note, width, fonts, 0, None))
        }
        "gopher" => {
            let text_page = protocols::fetch_gopher(url, chain, resolver, timeout)?;
            Ok(finish(url, text_page.html, 200, text_page.title, text_page.note, width, fonts, 0, None))
        }
        _ => {
            let response: Response = fetch_with(url, chain, resolver, timeout)?;
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
                Some(ImageFetch { chain, resolver, timeout }),
            ))
        }
    }
}

/// How to fetch a page's images: the page's own chain and resolver, so an
/// image request leaves exactly like the page request did.
struct ImageFetch<'a> {
    chain: &'a Chain,
    resolver: Option<&'a Resolver>,
    timeout: Duration,
}

const MAX_IMAGES: usize = 64;
const IMAGE_PARALLEL: usize = 6;
/// Images never hold a page back longer than this, whatever the network does.
const IMAGE_BUDGET: Duration = Duration::from_secs(8);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(6);

/// Requests not worth making: formats we cannot draw (SVG), and 1×1 images,
/// which show nothing and exist to report that the page was viewed.
fn worth_fetching(node: &dom::Node, url: &Url) -> bool {
    let tiny = |name: &str| node.attr(name).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).is_some_and(|v| v <= 1.0);
    if tiny("width") && tiny("height") {
        return false;
    }
    !url.path.to_ascii_lowercase().ends_with(".svg")
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
                        let bitmap = fetch_with(url, how.chain, how.resolver, per_image)
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
) -> Loaded {
    let document = dom::parse_document(&html);
    let mut sheet = Stylesheet::default();
    collect_styles(&document, &mut sheet);
    let rules = sheet.rules.len() + extra_rules;
    let title = {
        let from_doc = Layout::title(&document);
        if from_doc.is_empty() {
            title_hint
        } else {
            from_doc
        }
    };
    let images = match &fetch {
        Some(how) => {
            let (images, found, loaded) = fetch_images(&document, url, how);
            if found > 0 {
                note = format!("{note} · images {loaded}/{found}");
            }
            images
        }
        None => ImageMap::new(),
    };
    let page = Layout::new(fonts, &sheet, width).with_images(&images).build(dom::body(&document));
    Loaded {
        url: url.clone(),
        title,
        page,
        status,
        rules,
        note,
    }
}

pub fn collect_styles(document: &dom::Node, sheet: &mut Stylesheet) {
    fn walk(node: &dom::Node, sheet: &mut Stylesheet) {
        if node.tag == "style" {
            sheet.rules.extend(Stylesheet::parse(&node.text_content()).rules);
        }
        for child in &node.children {
            walk(child, sheet);
        }
    }
    walk(document, sheet);
}
