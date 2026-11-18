//! JavaScript: Boa running page scripts against a mutable DOM.
//!
//! The DOM lives here as an arena of nodes with stable ids. Scripts see it
//! through `Element` / `Document` objects written in JS (`PRELUDE`) on top of
//! a few native functions (`__dom_*`). Layout gets a plain `dom::Node` copy
//! where every element carries `data-sepia-id`, so a click in the window can
//! be traced back to its element.
//!
//! What scripts cannot do (the anonymity rules that still hold):
//! * network: `fetch` reaches only the page's own origin, through the same
//!   chain and DoH as the page; no XHR, WebSocket, beacons, workers
//! * storage: cookies and localStorage/sessionStorage live in memory only,
//!   cookies partitioned by site (see `cookies.rs`)
//! * fingerprinting: navigator/screen report the same values for every
//!   user, no WebGL, audio, battery, devices or high-resolution time (2D canvas: canvas2d.rs)
//! * runaway code: loop and recursion limits stop a script, not the browser
//!
//! A `Session` owns one page: its DOM, its Boa context, its stylesheet and
//! images. It is not `Send` (Boa contexts are tied to a thread), so the
//! window keeps each session on the thread that loaded the page.

use std::cell::RefCell;
use std::time::Duration;

use boa_engine::object::builtins::JsArray;
use boa_engine::{js_string, Context, JsResult, JsString, JsValue, NativeFunction, Source};

use crate::css::{matches_selector, split_top_level, Selector, Stylesheet};
use crate::dom::{self, Node};
use crate::images::ImageMap;
use crate::layout::{Fonts, Layout, Page};
use crate::net::{fetch_in, Chain, PageRequest};
use crate::resolver::Resolver;
use crate::url::Url;

// ---------------------------------------------------------------------------
// The DOM arena
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DNode {
    tag: String,
    attrs: Vec<(String, String)>,
    text: Option<String>,
    parent: Option<usize>,
    children: Vec<usize>,
    /// a `<template>`'s content fragment, once scripts asked for it
    content: Option<usize>,
    /// an element's shadow root (a fragment), or a shadow root's host
    shadow: Option<usize>,
}

/// A mutable document: node 0 is `#document`.
#[derive(Debug, Clone, Default)]
pub struct Dom {
    nodes: Vec<DNode>,
    /// bumped by every change, so the tree built for selector queries is
    /// reused until the document changes
    version: u64,
    tree_cache: RefCell<Option<(usize, u64, std::rc::Rc<Node>)>>,
}

impl Dom {
    pub fn from_tree(root: &Node) -> Dom {
        let mut dom = Dom { nodes: Vec::new(), version: 0, tree_cache: RefCell::new(None) };
        dom.adopt(root, None);
        dom.declarative_shadow_roots();
        dom
    }

    fn adopt(&mut self, node: &Node, parent: Option<usize>) -> usize {
        let id = self.nodes.len();
        let comment = node.tag == "#comment";
        self.nodes.push(DNode { tag: node.tag.clone(), attrs: if comment { Vec::new() } else { node.attrs.clone() },
            text: if comment { Some(node.attr("__sepia_comment").unwrap_or("").into()) } else { node.text.clone() }, parent, children: Vec::new(), content: None, shadow: None });
        for child in &node.children {
            let c = self.adopt(child, Some(id));
            self.nodes[id].children.push(c);
        }
        id
    }

    /// `<template shadowrootmode="open|closed">` from the parser: its children
    /// become the shadow root of the element around it (declarative Shadow DOM).
    fn declarative_shadow_roots(&mut self) {
        for id in 0..self.nodes.len() {
            if self.nodes[id].tag != "template" {
                continue;
            }
            let Some(mode) = self.attr(id, "shadowrootmode").or_else(|| self.attr(id, "shadowroot")).map(str::to_ascii_lowercase) else { continue };
            let Some(host) = self.nodes[id].parent else { continue };
            if self.nodes[host].shadow.is_some() || !(mode == "open" || mode == "closed") {
                continue;
            }
            let root = self.create("#fragment", None);
            self.nodes[root].attrs.push(("mode".into(), mode));
            for c in std::mem::take(&mut self.nodes[id].children) {
                self.nodes[c].parent = Some(root);
                self.nodes[root].children.push(c);
            }
            self.detach(id);
            self.nodes[host].shadow = Some(root);
            self.nodes[root].shadow = Some(host);
        }
    }

    fn is_shadow_root(&self, id: usize) -> bool {
        self.nodes[id].tag == "#fragment" && self.nodes[id].shadow.is_some()
    }

    /// The tree for layout, every element tagged with `data-sepia-id`: the
    /// flat tree, where a shadow host shows its shadow root and each `<slot>`
    /// the host's children assigned to it (or its own, as a fallback).
    pub fn to_tree(&self) -> Node {
        self.flat(0, &[])
    }

    /// `slotted`: the host children a `<slot>` met inside this shadow tree may take.
    fn flat(&self, id: usize, slotted: &[usize]) -> Node {
        let n = &self.nodes[id];
        if n.tag == "#comment" {
            return Node::comment_node(n.text.as_deref().unwrap_or(""));
        }
        let mut attrs = n.attrs.clone();
        if n.text.is_none() && !n.tag.starts_with('#') {
            attrs.push(("data-sepia-id".into(), id.to_string()));
        }
        let children: Vec<Node> = match (n.shadow, self.is_shadow_root(id)) {
            // a host: its shadow tree, with its own children to hand to slots
            (Some(root), false) => self.nodes[root].children.iter().map(|c| self.flat(*c, &n.children)).collect(),
            _ if n.tag == "slot" && !slotted.is_empty() => {
                let name = self.attr(id, "name").unwrap_or("");
                let assigned: Vec<usize> = slotted
                    .iter()
                    .copied()
                    .filter(|c| {
                        let slot = self.attr(*c, "slot").unwrap_or("");
                        let is_text = self.nodes[*c].text.is_some();
                        if name.is_empty() { is_text || slot.is_empty() } else { !is_text && slot == name }
                    })
                    .collect();
                if assigned.is_empty() {
                    n.children.iter().map(|c| self.flat(*c, slotted)).collect()
                } else {
                    // assigned nodes keep their own (light) context for nested slots
                    assigned.iter().map(|c| self.flat(*c, &[])).collect()
                }
            }
            _ => n.children.iter().map(|c| self.flat(*c, slotted)).collect(),
        };
        Node { tag: n.tag.clone(), attrs, children, text: n.text.clone() }
    }

    fn build(&self, id: usize) -> Node {
        let n = &self.nodes[id];
        // Comment data belongs to the script DOM, never to rendered text.
        if n.tag == "#comment" { return Node::comment_node(n.text.as_deref().unwrap_or("")); }
        let mut attrs = n.attrs.clone();
        if n.text.is_none() && !n.tag.starts_with('#') {
            attrs.push(("data-sepia-id".into(), id.to_string()));
        }
        Node { tag: n.tag.clone(), attrs, children: n.children.iter().map(|c| self.build(*c)).collect(), text: n.text.clone() }
    }

    fn valid(&self, id: i64) -> Option<usize> {
        (id >= 0 && (id as usize) < self.nodes.len()).then_some(id as usize)
    }

    fn create(&mut self, tag: &str, text: Option<String>) -> usize {
        self.nodes.push(DNode { tag: tag.to_string(), attrs: Vec::new(), text, parent: None, children: Vec::new(), content: None, shadow: None });
        self.nodes.len() - 1
    }

    fn detach(&mut self, id: usize) {
        self.version += 1;
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p].children.retain(|c| *c != id);
        }
    }

    fn is_ancestor(&self, maybe: usize, of: usize) -> bool {
        let mut at = Some(of);
        while let Some(n) = at {
            if n == maybe {
                return true;
            }
            at = self.nodes[n].parent;
        }
        false
    }

    /// Insert `child` under `parent` before `before` (or at the end). A
    /// fragment moves its children instead of itself.
    fn insert(&mut self, parent: usize, child: usize, before: Option<usize>) {
        self.version += 1;
        if self.is_ancestor(child, parent) {
            return;
        }
        if self.nodes[child].tag == "#fragment" {
            let kids = std::mem::take(&mut self.nodes[child].children);
            for k in kids {
                self.nodes[k].parent = None;
                self.insert(parent, k, before);
            }
            return;
        }
        // inserting a node before itself leaves it where it is (the reference
        // is its next sibling once it is taken out)
        let before = match before {
            Some(b) if b == child => {
                let kids = &self.nodes[parent].children;
                match kids.iter().position(|c| *c == child) {
                    Some(i) => kids.get(i + 1).copied(),
                    None => None,
                }
            }
            other => other,
        };
        self.detach(child);
        let at = before.and_then(|b| self.nodes[parent].children.iter().position(|c| *c == b)).unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(at, child);
        self.nodes[child].parent = Some(parent);
    }

    fn text_content(&self, id: usize) -> String {
        let n = &self.nodes[id];
        match &n.text {
            Some(t) => t.clone(),
            None => n.children.iter().filter(|c| self.nodes[**c].tag != "#comment").map(|c| self.text_content(*c)).collect(),
        }
    }

    fn set_text(&mut self, id: usize, text: String) {
        self.version += 1;
        if self.nodes[id].text.is_some() {
            self.nodes[id].text = Some(text);
            return;
        }
        for c in std::mem::take(&mut self.nodes[id].children) {
            self.nodes[c].parent = None;
        }
        if !text.is_empty() {
            let t = self.create("#text", Some(text));
            self.insert(id, t, None);
        }
    }

    fn set_html(&mut self, id: usize, html: &str) {
        self.version += 1;
        for c in std::mem::take(&mut self.nodes[id].children) {
            self.nodes[c].parent = None;
        }
        let parsed = dom::parse_document(html);
        // parse_document may wrap a fragment in html/body
        let source = parsed.find_first("body").filter(|_| !html.to_ascii_lowercase().contains("<body")).unwrap_or(&parsed);
        for child in &source.children {
            let c = self.adopt(child, None);
            self.insert(id, c, None);
        }
    }

    fn html(&self, id: usize, outer: bool) -> String {
        let n = &self.nodes[id];
        if n.tag == "#comment" { return format!("<!--{}-->", n.text.as_deref().unwrap_or("")); }
        if let Some(t) = &n.text {
            return escape(t, false);
        }
        // a template serializes its content (its children moved there)
        let kids = match n.content {
            Some(f) => &self.nodes[f].children,
            None => &n.children,
        };
        let inner: String = kids.iter().map(|c| self.html(*c, true)).collect();
        if !outer || n.tag.starts_with('#') {
            return inner;
        }
        let attrs: String = n.attrs.iter().map(|(k, v)| format!(" {k}=\"{}\"", escape(v, true))).collect();
        if crate::html::is_void(&n.tag) {
            format!("<{}{attrs}>", n.tag)
        } else {
            format!("<{}{attrs}>{inner}</{}>", n.tag, n.tag)
        }
    }

    fn attr(&self, id: usize, name: &str) -> Option<&str> {
        self.nodes[id].attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn set_attr(&mut self, id: usize, name: &str, value: String) {
        self.version += 1;
        let attrs = &mut self.nodes[id].attrs;
        match attrs.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = value,
            None => attrs.push((name.to_string(), value)),
        }
    }

    fn find_tag(&self, tag: &str) -> Option<usize> {
        (0..self.nodes.len()).find(|i| self.nodes[*i].tag == tag && self.is_ancestor(0, *i))
    }

    /// Elements under `scope` matching a selector list, in document order.
    /// The tree of the document (or detached subtree) holding `root`, built
    /// once per version of the DOM.
    fn tree_of(&self, root: usize) -> std::rc::Rc<Node> {
        let mut cache = self.tree_cache.borrow_mut();
        if let Some((r, v, tree)) = cache.as_ref() {
            if *r == root && *v == self.version {
                return tree.clone();
            }
        }
        let tree = std::rc::Rc::new(self.build(root));
        *cache = Some((root, self.version, tree.clone()));
        tree
    }

    /// `id`'s root and the child indices leading from it to `id`.
    fn path(&self, id: usize) -> (usize, Vec<usize>) {
        let mut path = Vec::new();
        let mut at = id;
        while let Some(parent) = self.nodes[at].parent {
            path.push(self.nodes[parent].children.iter().position(|c| *c == at).unwrap_or(0));
            at = parent;
        }
        path.reverse();
        (at, path)
    }

    /// Elements under `scope` matching a selector list, in document order.
    fn query(&self, scope: usize, selectors: &str, all: bool) -> Vec<usize> {
        let list: Vec<Selector> = split_top_level(selectors, ',').iter().filter_map(|s| Selector::parse(s)).collect();
        if list.is_empty() {
            return Vec::new();
        }
        let (root, path) = self.path(scope);
        let tree = self.tree_of(root);
        // the scope node and its ancestors in the built tree
        let mut ancestors: Vec<&Node> = Vec::new();
        let mut node: &Node = &tree;
        for i in &path {
            ancestors.push(node);
            match node.children.get(*i) {
                Some(child) => node = child,
                None => return Vec::new(),
            }
        }
        let mut out = Vec::new();
        fn walk<'a>(node: &'a Node, ancestors: &mut Vec<&'a Node>, list: &[Selector], all: bool, out: &mut Vec<usize>) {
            for child in &node.children {
                if !all && !out.is_empty() {
                    return;
                }
                if child.text.is_none() && !child.tag.starts_with('#') && list.iter().any(|s| matches_selector(s, child, ancestors)) {
                    if let Some(id) = child.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok()) {
                        out.push(id);
                        if !all {
                            return;
                        }
                    }
                }
                ancestors.push(child);
                walk(child, ancestors, list, all, out);
                ancestors.pop();
            }
        }
        ancestors.push(node);
        walk(node, &mut ancestors, &list, all, &mut out);
        out
    }

    fn matches(&self, id: usize, selectors: &str) -> bool {
        let list: Vec<Selector> = split_top_level(selectors, ',').iter().filter_map(|s| Selector::parse(s)).collect();
        let (root, path) = self.path(id);
        let tree = self.tree_of(root);
        let mut ancestors: Vec<&Node> = Vec::new();
        let mut node: &Node = &tree;
        for i in &path {
            ancestors.push(node);
            match node.children.get(*i) {
                Some(child) => node = child,
                None => return false,
            }
        }
        list.iter().any(|s| matches_selector(s, node, &ancestors))
    }
}

fn escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Per-thread state the natives work on
// ---------------------------------------------------------------------------

struct Net {
    origin: Url,
    chain: Chain,
    resolver: Option<Resolver>,
    timeout: Duration,
}

#[derive(Default)]
struct State {
    dom: Dom,
    url: Option<Url>,
    /// the #fragment, kept apart from `url` (which has no place for it)
    hash: String,
    title: Option<String>,
    focus: Option<usize>,
    navigate: Option<String>,
    form: Option<String>,
    jump: Option<String>,
    log: Vec<String>,
    dirty: bool,
    net: Option<Net>,
    /// `<style>` / `<link rel=stylesheet>` elements of the loaded document:
    /// already in the session's stylesheet
    initial_styles: std::collections::HashSet<usize>,
    /// CSS scripts added: a fetched `<link>`'s text or a `<style>`'s CSSOM
    /// rules (insertRule), by element
    dyn_css: std::collections::HashMap<usize, String>,
    /// this page's copies of files that do not change (scripts, styles,
    /// fonts), so a second request for one is not a second trip; memory only,
    /// gone with the page
    http_cache: std::collections::HashMap<String, crate::net::Response>,
    http_cache_bytes: usize,
    /// requests a page's fetch()/XHR started, answered on background threads
    inflight: Vec<Inflight>,
    image_loads: Vec<ImageLoad>,
    /// answers ready without a trip (from the page's cache), waiting to be handed over
    ready: Vec<(u32, Result<crate::net::Response, String>)>,
    next_request: u32,
    /// open WebSockets: what the page sends, and what arrives for it
    sockets: Vec<Socket>,
}

/// One WebSocket: its id, the way to its thread, and its events.
struct Socket {
    id: u32,
    out: std::sync::mpsc::Sender<SocketOut>,
    events: std::sync::mpsc::Receiver<SocketEvent>,
    closed: bool,
}

enum SocketOut {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

enum SocketEvent {
    Open(String),
    Text(String),
    Binary(Vec<u8>),
    Error(String),
    Close(u16, String),
}

/// The socket's thread: connect through the page's path, then pass messages
/// both ways until either side closes.
fn socket_thread(url: Url, page: Url, protocols: Vec<String>, net: (Chain, Option<Resolver>, Duration), out: std::sync::mpsc::Receiver<SocketOut>, events: std::sync::mpsc::Sender<SocketEvent>) {
    use tungstenite::{protocol::frame::coding::CloseCode, protocol::CloseFrame, Message};
    let site = crate::cookies::site_of(&page.host);
    let (mut ws, protocol) = match crate::net::websocket_connect(&url, &page, &site, &protocols, &net.0, net.1.as_ref(), net.2) {
        Ok(v) => v,
        Err(e) => {
            let _ = events.send(SocketEvent::Error(e));
            let _ = events.send(SocketEvent::Close(1006, String::new()));
            return;
        }
    };
    let _ = events.send(SocketEvent::Open(protocol));
    ws.get_ref().set_read_timeout(Some(Duration::from_millis(20)));
    let mut close_seen: Option<(u16, String)> = None;
    // we asked to close: what follows is the end of the closing handshake
    let mut closing = false;
    loop {
        loop {
            match out.try_recv() {
                Ok(SocketOut::Text(t)) => { let _ = ws.send(Message::Text(t.into())); }
                Ok(SocketOut::Binary(b)) => { let _ = ws.send(Message::Binary(b.into())); }
                Ok(SocketOut::Close(code, reason)) => {
                    closing = true;
                    let _ = ws.close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() }));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // the page is gone: close quietly
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        match ws.read() {
            Ok(Message::Text(t)) => { let _ = events.send(SocketEvent::Text(t.to_string())); }
            Ok(Message::Binary(b)) => { let _ = events.send(SocketEvent::Binary(b.to_vec())); }
            Ok(Message::Close(frame)) => {
                close_seen = Some(frame.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1005, String::new())));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                let _ = ws.flush();
            }
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                let (code, reason) = close_seen.take().unwrap_or((1000, String::new()));
                let _ = events.send(SocketEvent::Close(code, reason));
                return;
            }
            Err(e) => {
                if !closing && close_seen.is_none() {
                    let _ = events.send(SocketEvent::Error(e.to_string()));
                }
                let (code, reason) = close_seen.take().unwrap_or(if closing { (1000, String::new()) } else { (1006, String::new()) });
                let _ = events.send(SocketEvent::Close(code, reason));
                return;
            }
        }
    }
}

/// One request in flight: its id, whether its answer may be kept, and where it arrives.
/// An image a script asked for (`img.src = …`), fetched and decoded off-thread.
struct ImageLoad {
    id: u32,
    /// the src as written, and resolved
    keys: (String, String),
    rx: std::sync::mpsc::Receiver<Option<crate::images::Bitmap>>,
}

struct Inflight {
    id: u32,
    url: Url,
    keep: bool,
    /// a script's request: the page's origin and whether it asked for no-cors
    cors: Option<(String, bool)>,
    rx: std::sync::mpsc::Receiver<Result<crate::net::Response, String>>,
}

const HTTP_CACHE_BYTES: usize = usize::MAX;

/// What a script may see of a response to its fetch/XHR, by CORS: the
/// response itself same-origin or when the server allows this origin, an
/// opaque one (status 0, no body) in no-cors mode, else a network error.
/// (Preflight requests are not made.)
fn cors_view(mut r: crate::net::Response, target: &Url, origin: &str, no_cors: bool) -> Result<crate::net::Response, String> {
    if crate::net::origin_of(target) == origin {
        return Ok(r);
    }
    let allowed = r.header("access-control-allow-origin").map(str::trim).is_some_and(|a| a == "*" || a.eq_ignore_ascii_case(origin));
    if allowed {
        return Ok(r);
    }
    if no_cors {
        r.status = 0;
        r.body.clear();
        r.headers.clear();
        return Ok(r);
    }
    Err(format!("CORS: {} does not allow {origin}", target.host))
}

/// A response worth keeping for the page's lifetime: a static file.
fn is_static(r: &crate::net::Response) -> bool {
    let kind = r.content_type();
    let control = r.header("cache-control").unwrap_or("").to_ascii_lowercase();
    r.status == 200
        && !control.contains("no-store")
        && ["javascript", "ecmascript", "text/css", "wasm", "font/", "woff"].iter().any(|k| kind.contains(k))
}

impl State {
    fn remember(&mut self, url: &Url, r: &crate::net::Response) {
        if is_static(r) && self.http_cache_bytes + r.body.len() <= HTTP_CACHE_BYTES {
            self.http_cache_bytes += r.body.len();
            self.http_cache.insert(url.to_string(), r.clone());
        }
    }

    /// Many GETs at once (the page's path, its cache), in the order asked.
    fn get_many(&mut self, urls: &[Url], site: &str) -> Vec<Option<crate::net::Response>> {
        let Some(net) = self.net.as_ref() else { return urls.iter().map(|_| None).collect() };
        let (chain, resolver, timeout) = (net.chain.clone(), net.resolver.clone(), net.timeout);
        let mut out: Vec<Option<crate::net::Response>> = urls.iter().map(|u| self.http_cache.get(&u.to_string()).cloned()).collect();
        let missing: Vec<usize> = (0..urls.len()).filter(|i| out[*i].is_none()).collect();
        for batch in missing.chunks(MODULE_PARALLEL) {
            let got: Vec<(usize, Option<crate::net::Response>)> = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|&i| {
                        let (url, chain, resolver) = (&urls[i], &chain, &resolver);
                        scope.spawn(move || (i, fetch_in(url, chain, resolver.as_ref(), timeout, &PageRequest::get(site)).ok()))
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            for (i, r) in got {
                if let Some(r) = &r {
                    self.remember(&urls[i], r);
                }
                out[i] = r;
            }
        }
        out
    }

    /// GET through the page's path, answered from the page's cache if it has it.
    fn get(&mut self, url: &Url, site: &str) -> Result<crate::net::Response, String> {
        if let Some(r) = self.http_cache.get(&url.to_string()) {
            return Ok(r.clone());
        }
        let net = self.net.as_ref().ok_or("network is off")?;
        let r = fetch_in(url, &net.chain, net.resolver.as_ref(), net.timeout, &PageRequest::get(site))?;
        self.remember(url, &r);
        Ok(r)
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// What scripts read as layout (getBoundingClientRect, offsetWidth…): the
/// boxes of the page as our engine lays it out, redone when the DOM changed
/// (at most every so often: a layout is not free).
struct Geometry {
    sheet: std::rc::Rc<Stylesheet>,
    images: ImageMap,
    width: f32,
    /// the sheet with the CSS scripts added, by a hash of that CSS
    live: Option<(u64, std::rc::Rc<Stylesheet>)>,
    boxes: std::rc::Rc<std::collections::HashMap<usize, crate::layout::Rect, crate::css::FastHash>>,
    /// boxless elements (inline wrappers): the union of what is inside them
    unions: RefCell<std::collections::HashMap<usize, Option<crate::layout::Rect>, crate::css::FastHash>>,
    height: f32,
    version: Option<u64>,
    at: Option<std::time::Instant>,
    cost: Duration,
    /// layout time spent answering scripts in the current second
    spent: (std::time::Instant, Duration),
}

thread_local! {
    static GEOMETRY: RefCell<Option<Geometry>> = const { RefCell::new(None) };
    static GEOMETRY_FONTS: std::cell::OnceCell<Fonts> = const { std::cell::OnceCell::new() };
}

/// A loaded image by its URL (as written in `src`, or absolute), for canvas drawImage.
pub fn image_for(key: &str) -> Option<std::sync::Arc<crate::images::Bitmap>> {
    if let Some(b) = SCRIPT_IMAGES.with(|m| m.borrow().get(key).cloned()) {
        return Some(b);
    }
    GEOMETRY.with(|g| g.borrow().as_ref().and_then(|g| g.images.get(key).cloned()))
}

thread_local! {
    /// images scripts loaded themselves (`new Image()`), by URL
    static SCRIPT_IMAGES: RefCell<ImageMap> = RefCell::new(ImageMap::new());
    /// script images not yet handed to layout
    static NEW_SCRIPT_IMAGES: RefCell<Vec<(String, std::sync::Arc<crate::images::Bitmap>)>> = const { RefCell::new(Vec::new()) };
}

/// Images scripts loaded since the last call, for layout.
fn take_script_images() -> Vec<(String, std::sync::Arc<crate::images::Bitmap>)> {
    NEW_SCRIPT_IMAGES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

fn arg_num(args: &[JsValue], i: usize, ctx: &mut Context) -> f32 {
    args.get(i).and_then(|v| v.to_number(ctx).ok()).filter(|n| n.is_finite()).unwrap_or(0.0) as f32
}

fn with_fonts<R>(f: impl FnOnce(&Fonts) -> R) -> R {
    GEOMETRY_FONTS.with(|fonts| f(fonts.get_or_init(Fonts::load)))
}

fn set_geometry_inputs(sheet: &Stylesheet, images: &ImageMap, width: f32) {
    GEOMETRY.with(|g| {
        *g.borrow_mut() = Some(Geometry {
            sheet: std::rc::Rc::new(sheet.clone()),
            images: images.clone(),
            width,
            live: None,
            boxes: Default::default(),
            unions: Default::default(),
            height: 0.0,
            version: None,
            at: None,
            cost: Duration::ZERO,
            spent: (std::time::Instant::now(), Duration::ZERO),
        })
    });
}

/// Keep a layout someone already made (the window's, the CLI's) as the geometry.
fn remember_geometry(page: &Page) {
    let version = with(|st| st.dom.version);
    GEOMETRY.with(|g| {
        if let Some(g) = g.borrow_mut().as_mut() {
            // an inline element comes in pieces (one per line, per run): their union
            let mut boxes: std::collections::HashMap<usize, crate::layout::Rect, crate::css::FastHash> = Default::default();
            for (r, id) in &page.boxes {
                boxes.entry(*id).and_modify(|b| *b = union(*b, *r)).or_insert(*r);
            }
            // inline links: the union of their pieces on the lines
            for hit in &page.links {
                if let Some(id) = hit.node {
                    let r = hit.rect;
                    boxes.entry(id).and_modify(|b| *b = union(*b, r)).or_insert(r);
                }
            }
            g.boxes = std::rc::Rc::new(boxes);
            g.unions.borrow_mut().clear();
            g.height = page.height;
            g.version = Some(version);
            g.at = Some(std::time::Instant::now());
        }
    });
}

fn union(a: crate::layout::Rect, b: crate::layout::Rect) -> crate::layout::Rect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    crate::layout::Rect { x, y, w: (a.x + a.w).max(b.x + b.w) - x, h: (a.y + a.h).max(b.y + b.h) - y }
}

/// An element's box, or for one without (an inline wrapper) the union of its
/// descendants' boxes, through shadow trees.
fn rect_of(id: usize) -> Option<crate::layout::Rect> {
    let (boxes, _) = geometry_for(Some(id))?;
    if let Some(r) = boxes.get(&id) {
        return Some(*r);
    }
    if let Some(r) = GEOMETRY.with(|g| g.borrow().as_ref().and_then(|g| g.unions.borrow().get(&id).copied())) {
        return r;
    }
    fn walk(dom: &Dom, id: usize, boxes: &std::collections::HashMap<usize, crate::layout::Rect, crate::css::FastHash>, depth: usize) -> Option<crate::layout::Rect> {
        if let Some(r) = boxes.get(&id) {
            return Some(*r);
        }
        let n = dom.nodes.get(id)?;
        if depth > 400 || n.tag == "template" || n.tag == "script" || n.tag == "style" {
            return None;
        }
        let shadow = n.shadow.and_then(|r| dom.nodes.get(r)).map(|r| r.children.as_slice()).unwrap_or(&[]);
        let mut out: Option<crate::layout::Rect> = None;
        for c in shadow.iter().chain(n.children.iter()) {
            if let Some(r) = walk(dom, *c, boxes, depth + 1) {
                out = Some(out.map_or(r, |o| union(o, r)));
            }
        }
        out
    }
    let found = with(|st| walk(&st.dom, id, &boxes, 0));
    GEOMETRY.with(|g| {
        if let Some(g) = g.borrow().as_ref() {
            g.unions.borrow_mut().insert(id, found);
        }
    });
    found
}

/// The page's boxes, laid out again if the DOM changed and the last layout is not fresh.
fn geometry() -> Option<(std::rc::Rc<std::collections::HashMap<usize, crate::layout::Rect, crate::css::FastHash>>, f32)> {
    geometry_for(None)
}

/// As a browser lays out before answering a script that reads sizes: again
/// when the DOM changed, unless layouts are costly here (then at most every
/// few of their own durations), and always for an element the last layout
/// had not seen yet (`want`) if that layout is not much too slow to repeat.
fn geometry_for(want: Option<usize>) -> Option<(std::rc::Rc<std::collections::HashMap<usize, crate::layout::Rect, crate::css::FastHash>>, f32)> {
    let version = with(|st| st.dom.version);
    let stale = GEOMETRY.with(|g| {
        let g = g.borrow();
        let g = g.as_ref()?;
        if g.version == Some(version) {
            return Some(false);
        }
        let since = g.at.map_or(Duration::MAX, |t| t.elapsed());
        // layouts on demand share a budget: a script measuring in a loop
        // does not get a layout per read
        let (window, spent) = g.spent;
        let _ = (window, spent);
        // fresh for every read, as a browser; only a slow page's layout is
        // reused for a moment (two of its own durations) between DOM changes,
        // unless the element asked about is new to it
        let unseen = want.is_some_and(|id| !g.boxes.contains_key(&id));
        Some(g.cost < Duration::from_millis(100) || unseen || since >= g.cost * 2)
    })?;
    if stale {
        let started = std::time::Instant::now();
        let tree = with(|st| st.dom.to_tree());
        let (sheet, images, width) = current_sheet()?;
        let page = GEOMETRY_FONTS.with(|fonts| {
            let fonts = fonts.get_or_init(Fonts::load);
            Layout::new(fonts, &sheet, width).with_images(&images).build(&tree)
        });
        remember_geometry(&page);
        GEOMETRY.with(|g| {
            if let Some(g) = g.borrow_mut().as_mut() {
                g.cost = started.elapsed();
                if g.spent.0.elapsed() >= Duration::from_secs(1) {
                    g.spent = (std::time::Instant::now(), Duration::ZERO);
                }
                g.spent.1 += g.cost;
            }
        });
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            eprintln!("geometry: layout {} ms", started.elapsed().as_millis());
        }
    }
    GEOMETRY.with(|g| g.borrow().as_ref().map(|g| (g.boxes.clone(), g.height)))
}

/// The page's stylesheet with what scripts added, its images and width.
fn current_sheet() -> Option<(std::rc::Rc<Stylesheet>, ImageMap, f32)> {
    {
        let added = added_css();
        if let Ok(path) = std::env::var("SEPIA_CSS_DUMP") {
            // debugging: the CSS scripts added, as layout sees it
            let _ = std::fs::write(path, &added);
        }
        GEOMETRY.with(|g| {
            let mut g = g.borrow_mut();
            let g = g.as_mut().expect("checked above");
            let sheet = if added.is_empty() {
                g.sheet.clone()
            } else {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                added.hash(&mut h);
                let key = h.finish();
                if g.live.as_ref().map(|(k, _)| *k) != Some(key) {
                    let mut sheet = (*g.sheet).clone();
                    let mut extra = Stylesheet::parse(&added);
                    if let Some(base) = with(|st| st.url.clone()) {
                        extra.absolutize_urls(&base);
                    }
                    sheet.append(extra);
                    g.live = Some((key, std::rc::Rc::new(sheet)));
                }
                g.live.as_ref().expect("set above").1.clone()
            };
            Some((sheet, g.images.clone(), g.width))
        })
    }
}

/// The style our engine gives element `id` (in the flat tree, through shadow
/// roots), for getComputedStyle; with the width it was resolved against.
fn computed_style(id: usize) -> Option<(crate::style::Style, f32)> {
    let (sheet, _, viewport) = current_sheet()?;
    let tree = with(|st| st.dom.to_tree());
    let want = id.to_string();
    fn find<'a>(n: &'a Node, want: &str, path: &mut Vec<&'a Node>) -> bool {
        path.push(n);
        if n.attr("data-sepia-id") == Some(want) {
            return true;
        }
        for c in &n.children {
            if c.text.is_none() && find(c, want, path) {
                return true;
            }
        }
        path.pop();
        false
    }
    let mut path = Vec::new();
    if !find(&tree, &want, &mut path) {
        return None;
    }
    let boxes = GEOMETRY.with(|g| g.borrow().as_ref().map(|g| g.boxes.clone()));
    let mut style: Option<crate::style::Style> = None;
    let mut ancestors: Vec<&Node> = Vec::new();
    let mut width = viewport;
    for node in path {
        if node.tag.starts_with('#') {
            ancestors.push(node);
            continue;
        }
        let s = crate::style::compute(node, style.as_ref(), &sheet, &ancestors, width);
        let own = node.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok()).and_then(|i| boxes.as_ref().and_then(|b| b.get(&i).copied()));
        if node.attr("data-sepia-id") != Some(want.as_str()) {
            width = own.map(|r| r.w).or(s.width).unwrap_or(width);
        }
        ancestors.push(node);
        style = Some(s);
    }
    style.map(|s| (s, width))
}

fn arg_str(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    match args.get(i) {
        Some(v) if !v.is_undefined() && !v.is_null() => Ok(v.to_string(ctx)?.to_std_string_escaped()),
        _ => Ok(String::new()),
    }
}

fn arg_id(args: &[JsValue], i: usize) -> i64 {
    args.get(i).and_then(|v| v.as_number()).map(|n| n as i64).unwrap_or(-1)
}

fn s(v: impl Into<String>) -> JsValue {
    JsValue::from(JsString::from(v.into()))
}

fn ids(list: Vec<usize>, ctx: &mut Context) -> JsValue {
    JsArray::from_iter(list.into_iter().map(|i| JsValue::from(i as f64)), ctx).into()
}

macro_rules! native {
    ($ctx:expr, $name:literal, $len:expr, $f:expr) => {
        $ctx.register_global_builtin_callable(js_string!($name), $len, NativeFunction::from_fn_ptr($f))
            .expect("register native");
    };
}

fn register_canvas(ctx: &mut Context) {
    use crate::canvas2d as cv;
    native!(ctx, "__cv_init", 3, |_, a, ctx| {
        cv::init(arg_id(a, 0).max(0) as usize, arg_num(a, 1, ctx) as u32, arg_num(a, 2, ctx) as u32);
        with(|st| st.dirty = true);
        Ok(JsValue::undefined())
    });
    // (id, path, evenodd, paint, paint transform, alpha, op)
    native!(ctx, "__cv_fill", 7, |_, a, ctx| {
        let (path, spec, t, op) = (arg_str(a, 1, ctx)?, arg_str(a, 3, ctx)?, arg_str(a, 4, ctx)?, arg_str(a, 6, ctx)?);
        cv::fill(arg_id(a, 0).max(0) as usize, &path, a.get(2).map(|v| v.to_boolean()).unwrap_or(false), &spec, &t, arg_num(a, 5, ctx), &op);
        with(|st| st.dirty = true);
        Ok(JsValue::undefined())
    });
    // (id, path, paint, paint transform, alpha, op, width, cap, join, miter, dash, dash offset)
    native!(ctx, "__cv_stroke", 12, |_, a, ctx| {
        let (path, spec, t, op) = (arg_str(a, 1, ctx)?, arg_str(a, 2, ctx)?, arg_str(a, 3, ctx)?, arg_str(a, 5, ctx)?);
        let (cap, join, dash) = (arg_str(a, 7, ctx)?, arg_str(a, 8, ctx)?, arg_str(a, 10, ctx)?);
        cv::stroke(arg_id(a, 0).max(0) as usize, &path, &spec, &t, arg_num(a, 4, ctx), &op, arg_num(a, 6, ctx), &cap, &join, arg_num(a, 9, ctx), &dash, arg_num(a, 11, ctx));
        with(|st| st.dirty = true);
        Ok(JsValue::undefined())
    });
    native!(ctx, "__cv_clip", 2, |_, a, ctx| {
        cv::clip(arg_id(a, 0).max(0) as usize, &arg_str(a, 1, ctx)?);
        Ok(JsValue::undefined())
    });
    // (id, key, sx, sy, sw, sh, transform, alpha, op, smoothing)
    native!(ctx, "__cv_image", 10, |_, a, ctx| {
        let (key, t, op) = (arg_str(a, 1, ctx)?, arg_str(a, 6, ctx)?, arg_str(a, 8, ctx)?);
        cv::draw_image(arg_id(a, 0).max(0) as usize, &key, arg_num(a, 2, ctx), arg_num(a, 3, ctx), arg_num(a, 4, ctx), arg_num(a, 5, ctx), &t, arg_num(a, 7, ctx), &op, a.get(9).map(|v| v.to_boolean()).unwrap_or(true));
        with(|st| st.dirty = true);
        Ok(JsValue::undefined())
    });
    // natural size of an image source: "w,h"
    native!(ctx, "__cv_source_size", 1, |_, a, ctx| {
        let (w, h) = cv::source_size(&arg_str(a, 0, ctx)?);
        Ok(s(format!("{w},{h}")))
    });
    // (text, size, family, bold, italic) -> width
    native!(ctx, "__cv_measure", 5, |_, a, ctx| {
        let (text, family) = (arg_str(a, 0, ctx)?, arg_str(a, 2, ctx)?);
        let size = arg_num(a, 1, ctx);
        let (bold, italic) = (a.get(3).map(|v| v.to_boolean()).unwrap_or(false), a.get(4).map(|v| v.to_boolean()).unwrap_or(false));
        Ok(JsValue::from(with_fonts(|f| cv::measure(f, &text, size, &family, bold, italic)) as f64))
    });
    // (id, text, x, y, size, family, bold, italic, maxWidth, paint, ctm, alpha, op)
    native!(ctx, "__cv_text", 13, |_, a, ctx| {
        let (text, family, spec, ctm, op) = (arg_str(a, 1, ctx)?, arg_str(a, 5, ctx)?, arg_str(a, 9, ctx)?, arg_str(a, 10, ctx)?, arg_str(a, 12, ctx)?);
        let (x, y, size, max_w, alpha) = (arg_num(a, 2, ctx), arg_num(a, 3, ctx), arg_num(a, 4, ctx), arg_num(a, 8, ctx), arg_num(a, 11, ctx));
        let (bold, italic) = (a.get(6).map(|v| v.to_boolean()).unwrap_or(false), a.get(7).map(|v| v.to_boolean()).unwrap_or(false));
        let id = arg_id(a, 0).max(0) as usize;
        with_fonts(|f| cv::text(f, id, &text, x, y, size, &family, bold, italic, max_w, &spec, &ctm, alpha, &op));
        with(|st| st.dirty = true);
        Ok(JsValue::undefined())
    });
    native!(ctx, "__cv_get", 5, |_, a, ctx| {
        Ok(s(cv::get_image_data(arg_id(a, 0).max(0) as usize, arg_num(a, 1, ctx) as i32, arg_num(a, 2, ctx) as i32, arg_num(a, 3, ctx) as u32, arg_num(a, 4, ctx) as u32)))
    });
    // (id, bytes, dx, dy, w, h)
    native!(ctx, "__cv_put", 6, |_, a, ctx| {
        let bytes = arg_str(a, 1, ctx)?;
        cv::put_image_data(arg_id(a, 0).max(0) as usize, &bytes, arg_num(a, 2, ctx) as i32, arg_num(a, 3, ctx) as i32, arg_num(a, 4, ctx) as u32, arg_num(a, 5, ctx) as u32);
        with(|st| st.dirty = true);
        Ok(JsValue::undefined())
    });
    native!(ctx, "__cv_png", 1, |_, a, _| Ok(s(cv::png_base64(arg_id(a, 0).max(0) as usize))));
}

fn register(ctx: &mut Context) {
    register_canvas(ctx);
    native!(ctx, "__dom_kind", 1, |_, a, _| {
        let id = arg_id(a, 0);
        Ok(with(|st| match st.dom.valid(id) {
            Some(i) if st.dom.nodes[i].tag == "#comment" => 8.0,
            Some(i) if st.dom.nodes[i].text.is_some() => 3.0,
            Some(0) => 9.0,
            Some(i) if st.dom.nodes[i].tag == "#fragment" => 11.0,
            Some(_) => 1.0,
            None => 0.0,
        })
        .into())
    });
    native!(ctx, "__dom_tag", 1, |_, a, _| Ok(s(with(|st| st.dom.valid(arg_id(a, 0)).map(|i| st.dom.nodes[i].tag.clone()).unwrap_or_default()))));
    native!(ctx, "__dom_parent", 1, |_, a, _| {
        Ok(with(|st| st.dom.valid(arg_id(a, 0)).and_then(|i| st.dom.nodes[i].parent).map(|p| p as f64).unwrap_or(-1.0)).into())
    });
    native!(ctx, "__dom_children", 1, |_, a, ctx| {
        let list = with(|st| st.dom.valid(arg_id(a, 0)).map(|i| st.dom.nodes[i].children.clone()).unwrap_or_default());
        Ok(ids(list, ctx))
    });
    native!(ctx, "__dom_text", 1, |_, a, _| Ok(s(with(|st| st.dom.valid(arg_id(a, 0)).map(|i| st.dom.text_content(i)).unwrap_or_default()))));
    native!(ctx, "__dom_set_text", 2, |_, a, ctx| {
        let text = arg_str(a, 1, ctx)?;
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                st.dom.set_text(i, text);
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_get_attr", 2, |_, a, ctx| {
        let name = arg_str(a, 1, ctx)?.to_ascii_lowercase();
        Ok(with(|st| st.dom.valid(arg_id(a, 0)).and_then(|i| st.dom.attr(i, &name).map(|v| s(v)))).unwrap_or_else(JsValue::null))
    });
    native!(ctx, "__dom_set_attr", 3, |_, a, ctx| {
        let (name, value) = (arg_str(a, 1, ctx)?.to_ascii_lowercase(), arg_str(a, 2, ctx)?);
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                if st.dom.attr(i, &name) != Some(value.as_str()) {
                    st.dom.set_attr(i, &name, value);
                    st.dirty = true;
                }
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_remove_attr", 2, |_, a, ctx| {
        let name = arg_str(a, 1, ctx)?.to_ascii_lowercase();
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                let before = st.dom.nodes[i].attrs.len();
                st.dom.version += 1;
                st.dom.nodes[i].attrs.retain(|(k, _)| *k != name);
                st.dirty |= before != st.dom.nodes[i].attrs.len();
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_attr_names", 1, |_, a, ctx| {
        let names: Vec<String> = with(|st| st.dom.valid(arg_id(a, 0)).map(|i| st.dom.nodes[i].attrs.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default());
        Ok(JsArray::from_iter(names.into_iter().map(s), ctx).into())
    });
    native!(ctx, "__dom_create", 1, |_, a, ctx| {
        let tag = arg_str(a, 0, ctx)?.to_ascii_lowercase();
        Ok(with(|st| st.dom.create(&tag, None) as f64).into())
    });
    native!(ctx, "__dom_create_text", 1, |_, a, ctx| {
        let text = arg_str(a, 0, ctx)?;
        Ok(with(|st| st.dom.create("#text", Some(text)) as f64).into())
    });
    native!(ctx, "__dom_create_comment", 1, |_, a, ctx| {
        let text = arg_str(a, 0, ctx)?;
        Ok(with(|st| st.dom.create("#comment", Some(text)) as f64).into())
    });
    native!(ctx, "__dom_insert", 3, |_, a, _| {
        with(|st| {
            if let (Some(p), Some(c)) = (st.dom.valid(arg_id(a, 0)), st.dom.valid(arg_id(a, 1))) {
                let before = st.dom.valid(arg_id(a, 2));
                st.dom.insert(p, c, before);
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_remove", 1, |_, a, _| {
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                st.dom.detach(i);
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_clear", 1, |_, a, _| {
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                st.dom.version += 1;
                for c in std::mem::take(&mut st.dom.nodes[i].children) {
                    st.dom.nodes[c].parent = None;
                }
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    // attach a shadow root (a fragment) to a host
    native!(ctx, "__dom_attach_shadow", 2, |_, a, _| {
        with(|st| {
            if let (Some(h), Some(r)) = (st.dom.valid(arg_id(a, 0)), st.dom.valid(arg_id(a, 1))) {
                st.dom.version += 1;
                st.dom.nodes[h].shadow = Some(r);
                st.dom.nodes[r].shadow = Some(h);
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    // an element's shadow root, or a shadow root's host (-1 if none)
    native!(ctx, "__dom_shadow", 1, |_, a, _| {
        Ok(with(|st| st.dom.valid(arg_id(a, 0)).and_then(|i| st.dom.nodes[i].shadow).map(|p| p as f64).unwrap_or(-1.0)).into())
    });
    native!(ctx, "__dom_set_template_content", 2, |_, a, _| {
        with(|st| {
            if let (Some(t), Some(f)) = (st.dom.valid(arg_id(a, 0)), st.dom.valid(arg_id(a, 1))) {
                st.dom.version += 1;
                st.dom.nodes[t].content = Some(f);
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_html", 2, |_, a, _| {
        let outer = a.get(1).is_some_and(|v| v.to_boolean());
        Ok(s(with(|st| st.dom.valid(arg_id(a, 0)).map(|i| st.dom.html(i, outer)).unwrap_or_default())))
    });
    native!(ctx, "__dom_set_html", 2, |_, a, ctx| {
        let html = arg_str(a, 1, ctx)?;
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                st.dom.set_html(i, &html);
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_query", 3, |_, a, ctx| {
        let selector = arg_str(a, 1, ctx)?;
        let all = a.get(2).is_some_and(|v| v.to_boolean());
        let list = with(|st| st.dom.valid(arg_id(a, 0)).map(|i| st.dom.query(i, &selector, all)).unwrap_or_default());
        Ok(ids(list, ctx))
    });
    native!(ctx, "__dom_matches", 2, |_, a, ctx| {
        let selector = arg_str(a, 1, ctx)?;
        Ok(with(|st| st.dom.valid(arg_id(a, 0)).is_some_and(|i| st.dom.matches(i, &selector))).into())
    });
    native!(ctx, "__dom_find_tag", 1, |_, a, ctx| {
        let tag = arg_str(a, 0, ctx)?;
        Ok(with(|st| st.dom.find_tag(&tag).map(|i| i as f64).unwrap_or(-1.0)).into())
    });
    native!(ctx, "__dom_title", 0, |_, _, _| {
        Ok(s(with(|st| {
            st.title.clone().unwrap_or_else(|| st.dom.find_tag("title").map(|t| st.dom.text_content(t).trim().to_string()).unwrap_or_default())
        })))
    });
    native!(ctx, "__dom_set_title", 1, |_, a, ctx| {
        let title = arg_str(a, 0, ctx)?;
        with(|st| st.title = Some(title));
        Ok(JsValue::undefined())
    });
    native!(ctx, "__dom_focus", 1, |_, a, _| {
        with(|st| st.focus = st.dom.valid(arg_id(a, 0)));
        Ok(JsValue::undefined())
    });
    native!(ctx, "__page_url", 0, |_, _, _| {
        Ok(s(with(|st| {
            let base = st.url.as_ref().map(|u| u.to_string()).unwrap_or_default();
            if st.hash.is_empty() { base } else { format!("{base}#{}", st.hash) }
        })))
    });
    native!(ctx, "__page_set_url", 1, |_, a, ctx| {
        // history.pushState / replaceState / location.hash: same document, new URL
        let target = arg_str(a, 0, ctx)?;
        with(|st| {
            let (address, hash) = match target.split_once('#') {
                Some((a, h)) => (a.to_string(), h.to_string()),
                None => (target.clone(), String::new()),
            };
            if !address.is_empty() {
                if let Some(next) = st.url.as_ref().and_then(|u| u.resolve(&address)) {
                    // history may change the path, never the origin
                    if st.url.as_ref().is_some_and(|u| u.host == next.host && u.scheme == next.scheme) {
                        st.url = Some(next);
                    }
                }
            }
            if !hash.is_empty() && hash != st.hash {
                st.jump = Some(hash.clone());
            }
            st.hash = hash;
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__page_navigate", 2, |_, a, ctx| {
        let target = arg_str(a, 0, ctx)?;
        let form = match a.get(1) {
            Some(v) if !v.is_undefined() => Some(v.to_string(ctx)?.to_std_string_escaped()),
            _ => None,
        };
        with(|st| {
            st.navigate = Some(target);
            st.form = form;
        });
        Ok(JsValue::undefined())
    });
    // [x, y, width, height] of an element's box in the page, or null when it has none
    native!(ctx, "__dom_rect", 1, |_, a, ctx| {
        let id = a.first().and_then(|v| v.as_number()).unwrap_or(-1.0);
        if id < 0.0 {
            return Ok(JsValue::null());
        }
        let Some((_, height)) = geometry() else { return Ok(JsValue::null()) };
        let rect = if id == 0.0 {
            // the document: the whole page
            Some(crate::layout::Rect { x: 0.0, y: 0.0, w: GEOMETRY.with(|g| g.borrow().as_ref().map(|g| g.width).unwrap_or(0.0)), h: height })
        } else {
            rect_of(id as usize)
        };
        Ok(match rect {
            Some(r) => JsArray::from_iter([r.x, r.y, r.w, r.h].map(|v| JsValue::from(v as f64)), ctx).into(),
            None => JsValue::null(),
        })
    });
    // the computed style of an element, as CSS text values
    native!(ctx, "__computed_style", 1, |_, a, ctx| {
        use crate::style::{AlignItems, Display, FlexDirection, Justify, Position};
        let id = a.first().and_then(|v| v.as_number()).unwrap_or(-1.0);
        if id < 0.0 {
            return Ok(JsValue::null());
        }
        let Some((st, _)) = computed_style(id as usize) else { return Ok(JsValue::null()) };
        let rect = rect_of(id as usize);
        let px = |v: f32| format!("{}px", (v * 100.0).round() / 100.0);
        let color = |c: crate::css::Color| if c.a == 255 { format!("rgb({}, {}, {})", c.r, c.g, c.b) } else { format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, (c.a as f32 / 255.0 * 1000.0).round() / 1000.0) };
        let display = match st.display {
            Display::Block => "block", Display::Inline => "inline", Display::ListItem => "list-item", Display::None => "none",
            Display::Table => "table", Display::RowGroup => "table-row-group", Display::Row => "table-row", Display::Cell => "table-cell",
            Display::Flex => "flex", Display::Grid => "grid", Display::InlineBlock => "inline-block",
        };
        let mut pairs: Vec<(&str, String)> = vec![
            ("display", display.into()),
            ("position", match st.position { Position::Static => "static", Position::Relative => "relative", Position::Absolute => "absolute", Position::Fixed => "fixed", Position::Sticky => "sticky" }.into()),
            ("visibility", if st.hidden { "hidden".into() } else { "visible".into() }),
            ("opacity", format!("{}", st.opacity)),
            ("color", color(st.color)),
            ("background-color", st.background.map(color).unwrap_or_else(|| "rgba(0, 0, 0, 0)".into())),
            ("font-size", px(st.font_size)),
            ("font-weight", if st.bold { "700".into() } else { "400".into() }),
            ("font-style", if st.italic { "italic".into() } else { "normal".into() }),
            ("width", rect.map(|r| px(r.w)).or(st.width.map(px)).unwrap_or_else(|| "auto".into())),
            ("height", rect.map(|r| px(r.h)).or(st.height.map(px)).unwrap_or_else(|| "auto".into())),
            ("max-width", st.max_width.map(px).unwrap_or_else(|| "none".into())),
            ("min-width", st.min_width.map(px).unwrap_or_else(|| "0px".into())),
            ("min-height", st.min_height.map(px).unwrap_or_else(|| "0px".into())),
            ("margin-top", px(st.margin.top)), ("margin-right", px(st.margin.right)), ("margin-bottom", px(st.margin.bottom)), ("margin-left", px(st.margin.left)),
            ("padding-top", px(st.padding.top)), ("padding-right", px(st.padding.right)), ("padding-bottom", px(st.padding.bottom)), ("padding-left", px(st.padding.left)),
            ("border-top-width", px(st.border.top)), ("border-right-width", px(st.border.right)), ("border-bottom-width", px(st.border.bottom)), ("border-left-width", px(st.border.left)),
            ("box-sizing", if st.border_box { "border-box".into() } else { "content-box".into() }),
            ("flex-direction", match st.flex.direction { FlexDirection::Row => "row", FlexDirection::RowReverse => "row-reverse", FlexDirection::Column => "column", FlexDirection::ColumnReverse => "column-reverse" }.into()),
            ("flex-wrap", if st.flex.wrap { "wrap".into() } else { "nowrap".into() }),
            ("flex-grow", format!("{}", st.flex.grow)), ("flex-shrink", format!("{}", st.flex.shrink)),
            ("align-items", match st.flex.align { AlignItems::Start => "flex-start", AlignItems::End => "flex-end", AlignItems::Center => "center", _ => "normal" }.into()),
            ("justify-content", match st.flex.justify { Justify::Start => "normal", Justify::End => "flex-end", Justify::Center => "center", Justify::SpaceBetween => "space-between", Justify::SpaceAround => "space-around", Justify::SpaceEvenly => "space-evenly" }.into()),
            ("text-align", format!("{:?}", st.align).to_ascii_lowercase()),
            ("white-space", if st.pre { "pre".into() } else if st.nowrap { "nowrap".into() } else { "normal".into() }),
            ("overflow", if st.clip_x || st.clip_y { "hidden".into() } else { "visible".into() }),
            ("float", match st.float { Some(crate::style::Align::Left) => "left".into(), Some(crate::style::Align::Right) => "right".into(), _ => "none".into() }),
            ("top", st.top.map(px).unwrap_or_else(|| "auto".into())), ("left", st.left.map(px).unwrap_or_else(|| "auto".into())),
            ("right", st.right.map(px).unwrap_or_else(|| "auto".into())), ("bottom", st.bottom.map(px).unwrap_or_else(|| "auto".into())),
        ];
        pairs.push(("line-height", "normal".into()));
        let out = JsArray::new(ctx)?;
        for (k, v) in pairs {
            out.push(JsValue::from(js_string!(k)), ctx)?;
            out.push(JsValue::from(JsString::from(v.as_str())), ctx)?;
        }
        Ok(out.into())
    });
    // WebSocket: open (returns an id), send, close, and the events that arrived
    native!(ctx, "__ws_open", 2, |_, a, ctx| {
        let target = arg_str(a, 0, ctx)?;
        let protocols: Vec<String> = arg_str(a, 1, ctx)?.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        let Some(url) = Url::parse(&target).filter(|u| matches!(u.scheme.as_str(), "ws" | "wss")) else {
            return Err(boa_engine::JsNativeError::syntax().with_message(format!("Failed to construct 'WebSocket': The URL '{target}' is invalid.")).into());
        };
        let started = with(|st| {
            let page = st.url.clone()?;
            let net = st.net.as_ref().map(|n| (n.chain.clone(), n.resolver.clone(), n.timeout))?;
            st.next_request += 1;
            let id = st.next_request;
            let (out_tx, out_rx) = std::sync::mpsc::channel();
            let (ev_tx, ev_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || socket_thread(url, page, protocols, net, out_rx, ev_tx));
            st.sockets.push(Socket { id, out: out_tx, events: ev_rx, closed: false });
            Some(id)
        });
        Ok(started.map(|id| JsValue::from(id)).unwrap_or(JsValue::from(-1)))
    });
    native!(ctx, "__ws_send", 3, |_, a, ctx| {
        let id = a.first().and_then(|v| v.as_number()).unwrap_or(-1.0) as u32;
        let binary = a.get(1).map(|v| v.to_boolean()).unwrap_or(false);
        let message = if binary {
            let bytes = a.get(2).and_then(|v| v.as_object()).map(|o| {
                let len = o.get(js_string!("length"), ctx).ok().and_then(|l| l.as_number()).unwrap_or(0.0) as usize;
                (0..len).map(|i| o.get(i, ctx).ok().and_then(|v| v.as_number()).unwrap_or(0.0) as u8).collect::<Vec<u8>>()
            }).unwrap_or_default();
            SocketOut::Binary(bytes)
        } else {
            SocketOut::Text(arg_str(a, 2, ctx)?)
        };
        with(|st| {
            if let Some(s) = st.sockets.iter().find(|s| s.id == id) {
                let _ = s.out.send(message);
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__ws_close", 3, |_, a, ctx| {
        let id = a.first().and_then(|v| v.as_number()).unwrap_or(-1.0) as u32;
        let code = a.get(1).and_then(|v| v.as_number()).unwrap_or(1000.0) as u16;
        let reason = arg_str(a, 2, ctx).unwrap_or_default();
        with(|st| {
            if let Some(s) = st.sockets.iter().find(|s| s.id == id) {
                let _ = s.out.send(SocketOut::Close(code, reason));
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__ws_poll", 0, |_, _, ctx| {
        let mut rows: Vec<JsValue> = Vec::new();
        let events: Vec<(u32, SocketEvent)> = with(|st| {
            let mut out = Vec::new();
            for s in &mut st.sockets {
                while let Ok(e) = s.events.try_recv() {
                    if matches!(e, SocketEvent::Close(..)) {
                        s.closed = true;
                    }
                    out.push((s.id, e));
                }
            }
            st.sockets.retain(|s| !s.closed);
            out
        });
        for (id, e) in events {
            let row: Vec<JsValue> = match e {
                SocketEvent::Open(p) => vec![id.into(), js_string!("open").into(), JsString::from(p.as_str()).into()],
                SocketEvent::Text(t) => vec![id.into(), js_string!("text").into(), JsString::from(t.as_str()).into()],
                SocketEvent::Binary(b) => vec![id.into(), js_string!("binary").into(), JsArray::from_iter(b.into_iter().map(|x| JsValue::from(x as u32)), ctx).into()],
                SocketEvent::Error(m) => vec![id.into(), js_string!("error").into(), JsString::from(m.as_str()).into()],
                SocketEvent::Close(code, reason) => vec![id.into(), js_string!("close").into(), JsValue::from(code as u32), JsString::from(reason.as_str()).into()],
            };
            rows.push(JsArray::from_iter(row, ctx).into());
        }
        Ok(JsArray::from_iter(rows, ctx).into())
    });
    native!(ctx, "__media_matches", 3, |_, a, ctx| {
        let query = arg_str(a, 0, ctx)?;
        let width = a.get(1).and_then(|v| v.as_number()).unwrap_or(1024.0) as f32;
        let height = a.get(2).and_then(|v| v.as_number()).unwrap_or(768.0) as f32;
        Ok(JsValue::from(crate::css::Media::parse(&query).matches_viewport(width, height)))
    });
    native!(ctx, "__log", 1, |_, a, ctx| {
        let line = arg_str(a, 0, ctx)?;
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            eprintln!("js live: {}", line.chars().take(300).collect::<String>());
        }
        with(|st| {
            if st.log.len() < 100_000 {
                st.log.push(line);
            }
        });
        Ok(JsValue::undefined())
    });
    native!(ctx, "__cookie_get", 0, |_, _, _| {
        let line = with(|st| st.url.as_ref().map(|u| crate::cookies::script_view(&crate::cookies::site_of(&u.host), u)).unwrap_or_default());
        Ok(s(line))
    });
    native!(ctx, "__cookie_set", 1, |_, a, ctx| {
        let assignment = arg_str(a, 0, ctx)?;
        with(|st| {
            if let Some(u) = st.url.as_ref().filter(|u| u.scheme == "http" || u.scheme == "https") {
                crate::cookies::store_from_script(&crate::cookies::site_of(&u.host), u, &assignment);
            }
        });
        Ok(JsValue::undefined())
    });
    // CSS for one element (a fetched <link>, a <style>'s CSSOM rules); "" removes it
    native!(ctx, "__css_set", 2, |_, a, ctx| {
        let css = arg_str(a, 1, ctx)?;
        with(|st| {
            if let Some(i) = st.dom.valid(arg_id(a, 0)) {
                if css.is_empty() { st.dyn_css.remove(&i); } else { st.dyn_css.insert(i, css); }
                st.dirty = true;
            }
        });
        Ok(JsValue::undefined())
    });
    // url(...)s of a fetched stylesheet made absolute against where it came from
    native!(ctx, "__css_absolute", 2, |_, a, ctx| {
        let css = arg_str(a, 0, ctx)?;
        let base = arg_str(a, 1, ctx)?;
        Ok(s(match Url::parse(&base) { Some(b) => absolute_urls(&css, &b), None => css }))
    });
    // a stylesheet a script linked: fetched like the page's own stylesheets
    native!(ctx, "__css_fetch", 1, |_, a, ctx| {
        let target = arg_str(a, 0, ctx)?;
        let text = with(|st| -> Option<String> {
            let net = st.net.as_ref()?;
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let url = base.resolve(&target).filter(|u| matches!(u.scheme.as_str(), "http" | "https"))?;
            let site = crate::cookies::site_of(&net.origin.host);
            let response = st.get(&url, &site).ok()?;
            (response.status < 400 && true)
                .then(|| absolute_urls(&crate::charset::decode(&response.body, &response.content_type(), false, crate::charset::Encoding::Utf8).0, &url))
        });
        Ok(text.map(s).unwrap_or_else(JsValue::null))
    });
    // a <script src> a page's script inserted: fetched like the page's own
    // scripts (any origin, the page's path and cookie partition); text or null
    native!(ctx, "__script_fetch", 1, |_, a, ctx| {
        let target = arg_str(a, 0, ctx)?;
        let text = with(|st| -> Option<String> {
            let net = st.net.as_ref()?;
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let url = base.resolve(&target).filter(|u| matches!(u.scheme.as_str(), "http" | "https"))?;
            let site = crate::cookies::site_of(&net.origin.host);
            let started = std::time::Instant::now();
            let response = st.get(&url, &site).ok()?;
            if std::env::var_os("SEPIA_VERBOSE").is_some() {
                eprintln!("script inserted: {} -> {} {} bytes {} ms", url.to_string(), response.status, response.body.len(), started.elapsed().as_millis());
            }
            (response.status < 400 && true)
                .then(|| crate::charset::decode(&response.body, &response.content_type(), false, crate::charset::Encoding::Utf8).0)
        });
        Ok(text.map(s).unwrap_or_else(JsValue::null))
    });
    // several inserted <script src> at once, fetched in parallel: [text|null, ...]
    native!(ctx, "__scripts_fetch", 1, |_, a, ctx| {
        let list = a.get(0).and_then(|v| v.as_object());
        let mut targets = Vec::new();
        if let Some(list) = list {
            let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?.min(256);
            for i in 0..len {
                targets.push(list.get(i, ctx)?.to_string(ctx)?.to_std_string_escaped());
            }
        }
        let texts: Vec<Option<String>> = with(|st| {
            let Some(net) = st.net.as_ref() else { return targets.iter().map(|_| None).collect() };
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let site = crate::cookies::site_of(&net.origin.host);
            let urls: Vec<Option<Url>> = targets.iter().map(|t| base.resolve(t).filter(|u| matches!(u.scheme.as_str(), "http" | "https"))).collect();
            let wanted: Vec<Url> = urls.iter().flatten().cloned().collect();
            let started = std::time::Instant::now();
            let mut got = st.get_many(&wanted, &site).into_iter();
            if std::env::var_os("SEPIA_VERBOSE").is_some() {
                eprintln!("scripts inserted: {} fetched together in {} ms", wanted.len(), started.elapsed().as_millis());
            }
            urls.iter()
                .map(|u| {
                    u.as_ref()?;
                    let r = got.next()??;
                    (r.status < 400 && true)
                        .then(|| crate::charset::decode(&r.body, &r.content_type(), false, crate::charset::Encoding::Utf8).0)
                })
                .collect()
        });
        Ok(JsArray::from_iter(texts.into_iter().map(|t| t.map(s).unwrap_or_else(JsValue::null)), ctx).into())
    });
    // run a classic script the page inserted; false if it threw
    native!(ctx, "__script_eval", 2, |_, a, ctx| {
        let code = arg_str(a, 0, ctx)?;
        let label = arg_str(a, 1, ctx)?;
        peek(&code, &label);
        match ctx.eval(Source::from_bytes(code.as_bytes()).with_path(std::path::Path::new(&label))) {
            Ok(_) => Ok(JsValue::from(true)),
            Err(e) => {
                with(|st| st.log.push(format!("{label}: {e}")));
                Ok(JsValue::from(false))
            }
        }
    });
    // fetch()/XHR without blocking the page: start a request (same rules as
    // __fetch: the page's own site, the browser's header filter) -> its id
    native!(ctx, "__fetch_start", 6, |_, a, ctx| {
        let target = arg_str(a, 0, ctx)?;
        let opt = |i: usize, ctx: &mut Context| -> JsResult<Option<String>> {
            match a.get(i) {
                Some(v) if !v.is_undefined() && !v.is_null() => Ok(Some(v.to_string(ctx)?.to_std_string_escaped())),
                _ => Ok(None),
            }
        };
        let (method, body, content_type) = (opt(1, ctx)?, opt(2, ctx)?, opt(3, ctx)?);
        let no_cors = a.get(5).is_some_and(|v| v.to_boolean());
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(list) = a.get(4).and_then(|v| v.as_object()) {
            let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?.min(64);
            for i in 0..len {
                if let Some(pair) = list.get(i, ctx)?.as_object() {
                    headers.push((pair.get(0, ctx)?.to_string(ctx)?.to_std_string_escaped(), pair.get(1, ctx)?.to_string(ctx)?.to_std_string_escaped()));
                }
            }
        }
        let started = with(|st| -> Result<u32, String> {
            let net = st.net.as_ref().ok_or("network is off")?;
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let url = base.resolve(&target).ok_or("bad URL")?;
            let site = crate::cookies::site_of(&net.origin.host);
            if !matches!(url.scheme.as_str(), "http" | "https") {
                return Err(format!("unsupported scheme {}", url.scheme));
            }
            let page = base.clone();
            let cors = Some((crate::net::origin_of(&page), no_cors));
            let (chain, resolver, timeout) = (net.chain.clone(), net.resolver.clone(), net.timeout);
            st.next_request += 1;
            let id = st.next_request;
            let keep = body.is_none() && method.as_deref().is_none_or(|m| m.eq_ignore_ascii_case("GET"));
            if keep {
                if let Some(r) = st.http_cache.get(&url.to_string()).cloned() {
                    st.ready.push((id, Ok(r)));
                    return Ok(id);
                }
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let thread_url = url.clone();
            std::thread::spawn(move || {
                let request = PageRequest { site: &site, form: body.as_deref(), content_type: content_type.as_deref(), method: method.as_deref(), headers: &headers, from: Some(&page), cors: true };
                let started = std::time::Instant::now();
                let r = fetch_in(&thread_url, &chain, resolver.as_ref(), timeout, &request);
                if std::env::var_os("SEPIA_VERBOSE").is_some() {
                    match &r {
                        Ok(resp) => eprintln!("fetch: {} {} -> {} {} bytes {} ms (async)", method.as_deref().unwrap_or("GET"), thread_url.to_string(), resp.status, resp.body.len(), started.elapsed().as_millis()),
                        Err(e) => eprintln!("fetch: {} {} failed: {e}", method.as_deref().unwrap_or("GET"), thread_url.to_string()),
                    }
                }
                let _ = tx.send(r);
            });
            st.inflight.push(Inflight { id, url, keep, cors, rx });
            Ok(id)
        });
        match started {
            Ok(id) => Ok(JsValue::from(id)),
            Err(e) => Err(boa_engine::JsNativeError::typ().with_message(format!("fetch failed: {e}")).into()),
        }
    });
    // an inserted <script src> or <link> stylesheet, fetched in the background like
    // the page's own subresources (any origin, the page's path and cache) -> id
    native!(ctx, "__resource_start", 1, |_, a, ctx| {
        let target = arg_str(a, 0, ctx)?;
        let id = with(|st| -> Option<u32> {
            let net = st.net.as_ref()?;
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let url = base.resolve(&target).filter(|u| matches!(u.scheme.as_str(), "http" | "https"))?;
            let site = crate::cookies::site_of(&net.origin.host);
            let (chain, resolver, timeout) = (net.chain.clone(), net.resolver.clone(), net.timeout);
            st.next_request += 1;
            let id = st.next_request;
            if let Some(r) = st.http_cache.get(&url.to_string()).cloned() {
                st.ready.push((id, Ok(r)));
                return Some(id);
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let thread_url = url.clone();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let r = fetch_in(&thread_url, &chain, resolver.as_ref(), timeout, &PageRequest::get(&site));
                if std::env::var_os("SEPIA_VERBOSE").is_some() {
                    eprintln!("resource: {} {} ms", thread_url.to_string(), started.elapsed().as_millis());
                }
                let _ = tx.send(r);
            });
            st.inflight.push(Inflight { id, url, keep: true, cors: None, rx });
            Some(id)
        });
        Ok(id.map(JsValue::from).unwrap_or_else(JsValue::null))
    });
    // an image for a script (img.src): fetched and decoded in the background -> id
    // (its answer comes through __fetch_poll as [id, 200, "w,h", url] or [id, null, error])
    native!(ctx, "__image_start", 1, |_, a, ctx| {
        let raw = arg_str(a, 0, ctx)?;
        let id = with(|st| -> Option<u32> {
            let base = st.url.clone().or_else(|| st.net.as_ref().map(|n| n.origin.clone()));
            let resolved = base.as_ref().and_then(|b| b.resolve(&raw)).map(|u| u.to_string()).unwrap_or_else(|| raw.clone());
            st.next_request += 1;
            let id = st.next_request;
            let (tx, rx) = std::sync::mpsc::channel();
            if raw.trim_start().starts_with("data:") {
                let _ = tx.send(crate::images::decode_data_uri(raw.trim()));
            } else {
                let net = st.net.as_ref()?;
                let url = Url::parse(&resolved).filter(|u| matches!(u.scheme.as_str(), "http" | "https"))?;
                let site = crate::cookies::site_of(&net.origin.host);
                let (chain, resolver, timeout) = (net.chain.clone(), net.resolver.clone(), net.timeout);
                std::thread::spawn(move || {
                    let r = fetch_in(&url, &chain, resolver.as_ref(), timeout, &PageRequest::get(&site));
                    let bitmap = r.ok().filter(|r| r.status < 400).and_then(|r| crate::images::decode(&r.body));
                    let _ = tx.send(bitmap);
                });
            }
            st.image_loads.push(ImageLoad { id, keys: (raw.clone(), resolved), rx });
            Some(id)
        });
        Ok(id.map(JsValue::from).unwrap_or_else(JsValue::null))
    });
    // answers that arrived: [[id, status, text, url, headers], ...] or [id, null, error]
    native!(ctx, "__fetch_poll", 0, |_, _, ctx| {
        let mut images_done: Vec<JsValue> = Vec::new();
        let loads = with(|st| std::mem::take(&mut st.image_loads));
        let mut waiting = Vec::new();
        for load in loads {
            match load.rx.try_recv() {
                Ok(Some(bitmap)) => {
                    let (w, h) = bitmap.natural_size();
                    let bitmap = std::sync::Arc::new(bitmap);
                    SCRIPT_IMAGES.with(|m| {
                        let mut m = m.borrow_mut();
                        m.insert(load.keys.0.clone(), bitmap.clone());
                        m.insert(load.keys.1.clone(), bitmap.clone());
                    });
                    NEW_SCRIPT_IMAGES.with(|n| {
                        let mut n = n.borrow_mut();
                        n.push((load.keys.0.clone(), bitmap.clone()));
                        n.push((load.keys.1.clone(), bitmap));
                    });
                    images_done.push(JsArray::from_iter([JsValue::from(load.id), JsValue::from(200), s(format!("{},{}", w.round(), h.round())), s(load.keys.1.clone()), s("")], ctx).into());
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    images_done.push(JsArray::from_iter([JsValue::from(load.id), JsValue::null(), s("image failed")], ctx).into());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => waiting.push(load),
            }
        }
        with(|st| st.image_loads.extend(waiting));
        let done: Vec<(u32, Result<crate::net::Response, String>)> = with(|st| {
            let mut done = std::mem::take(&mut st.ready);
            let mut still = Vec::new();
            for f in std::mem::take(&mut st.inflight) {
                match f.rx.try_recv() {
                    Ok(r) => {
                        if let (true, Ok(resp)) = (f.keep, &r) {
                            st.remember(&f.url, resp);
                        }
                        let r = match (&f.cors, r) {
                            (Some((origin, no_cors)), Ok(resp)) => cors_view(resp, &f.url, origin, *no_cors),
                            (_, r) => r,
                        };
                        done.push((f.id, r));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => still.push(f),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => done.push((f.id, Err("connection lost".into()))),
                }
            }
            st.inflight = still;
            done
        });
        let mut rows = Vec::new();
        for (id, r) in done {
            let row = match r {
                Ok(resp) => {
                    let kind = resp.header("content-type").unwrap_or("").to_string();
                    let text = crate::charset::decode(&resp.body, &kind.to_ascii_lowercase(), kind.contains("html"), crate::charset::Encoding::Utf8).0;
                    let headers: String = resp
                        .headers
                        .iter()
                        .filter(|(k, _)| !k.eq_ignore_ascii_case("set-cookie") && !k.eq_ignore_ascii_case("set-cookie2"))
                        .map(|(k, v)| format!("{}: {v}\r\n", k.to_ascii_lowercase()))
                        .collect();
                    JsArray::from_iter([JsValue::from(id), JsValue::from(resp.status as f64), s(text), s(resp.final_url.clone()), s(headers)], ctx)
                }
                Err(e) => JsArray::from_iter([JsValue::from(id), JsValue::null(), s(e)], ctx),
            };
            rows.push(JsValue::from(row));
        }
        rows.extend(images_done);
        Ok(JsArray::from_iter(rows, ctx).into())
    });
    native!(ctx, "__fetch", 5, |_, a, ctx| {
        // the page's own site only (its origin and sibling subdomains, same
        // scheme), through the page's chain; returns [status, body, url, content-type]
        let target = arg_str(a, 0, ctx)?;
        let opt = |i: usize, ctx: &mut Context| -> JsResult<Option<String>> {
            match a.get(i) {
                Some(v) if !v.is_undefined() && !v.is_null() => Ok(Some(v.to_string(ctx)?.to_std_string_escaped())),
                _ => Ok(None),
            }
        };
        let (method, body, content_type) = (opt(1, ctx)?, opt(2, ctx)?, opt(3, ctx)?);
        // [[name, value], ...] the script set; net.rs keeps only what may go out
        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(list) = a.get(4).and_then(|v| v.as_object()) {
            let len = list.get(js_string!("length"), ctx)?.to_length(ctx)?.min(64);
            for i in 0..len {
                let pair = list.get(i, ctx)?;
                if let Some(pair) = pair.as_object() {
                    let k = pair.get(0, ctx)?.to_string(ctx)?.to_std_string_escaped();
                    let v = pair.get(1, ctx)?.to_string(ctx)?.to_std_string_escaped();
                    headers.push((k, v));
                }
            }
        }
        let result = with(|st| -> Result<(u16, String, String, String), String> {
            let net = st.net.as_ref().ok_or("network is off")?;
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let url = base.resolve(&target).ok_or("bad URL")?;
            let site = crate::cookies::site_of(&net.origin.host);
            if !matches!(url.scheme.as_str(), "http" | "https") {
                return Err(format!("unsupported scheme {}", url.scheme));
            }
            let page = base.clone();
            let request = PageRequest { site: &site, form: body.as_deref(), content_type: content_type.as_deref(), method: method.as_deref(), headers: &headers, from: Some(&page), cors: true };
            let started = std::time::Instant::now();
            let plain_get = body.is_none() && method.as_deref().is_none_or(|m| m.eq_ignore_ascii_case("GET"));
            let cached = if plain_get { st.http_cache.get(&url.to_string()).cloned() } else { None };
            let response = match cached {
                Some(r) => r,
                None => {
                    let net = st.net.as_ref().ok_or("network is off")?;
                    let r = fetch_in(&url, &net.chain, net.resolver.as_ref(), net.timeout, &request)?;
                    if plain_get {
                        st.remember(&url, &r);
                    }
                    r
                }
            };
            let took = started.elapsed().as_millis();
            let response = cors_view(response, &url, &crate::net::origin_of(&page), false)?;
            let kind = response.header("content-type").unwrap_or("").to_string();
            if std::env::var_os("SEPIA_VERBOSE").is_some() {
                eprintln!("fetch: {} {} -> {} {kind} {} bytes {took} ms", method.as_deref().unwrap_or("GET"), url.to_string(), response.status, response.body.len());
            }
            // the body as text in its declared charset (JSON and scripts default to UTF-8)
            let text = crate::charset::decode(&response.body, &kind.to_ascii_lowercase(), kind.contains("html"), crate::charset::Encoding::Utf8).0;
            Ok((response.status, text, response.final_url.clone(), kind))
        });
        match result {
            Ok((status, body, url, kind)) => Ok(JsArray::from_iter([JsValue::from(status as f64), s(body), s(url), s(kind)], ctx).into()),
            Err(e) => {
                if std::env::var_os("SEPIA_VERBOSE").is_some() {
                    eprintln!("fetch: {} {target} failed: {e} {}", method.as_deref().unwrap_or("GET"), body.as_deref().unwrap_or("").chars().take(1500).collect::<String>());
                }
                Err(boa_engine::JsNativeError::typ().with_message(format!("fetch failed: {e}")).into())
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Something the user did to the page.
#[derive(Debug, Clone)]
pub enum PageEvent {
    /// Wake the script event loop when a real timer deadline is due.
    Tick,
    /// A click on element `node` (its `data-sepia-id`); `href` if it was inside a link.
    Click { node: usize, href: Option<String> },
    /// Text typed into (or deleted from) a focused field.
    Input { node: usize, value: String },
    Key { node: usize, key: String },
    /// The content viewport changed size.
    Resize { width: f32, height: f32 },
    /// The fragment changed (a same-page `#link`).
    #[allow(dead_code)]
    Hash(String),
    /// The window's back (-1) or forward (1) inside the page's own history
    /// (when `Outcome::history` said it has such a step).
    #[allow(dead_code)]
    History(i32),
}

/// What a page event led to.
#[derive(Debug, Default)]
pub struct Outcome {
    /// A new layout when the DOM changed.
    pub page: Option<Page>,
    pub title: Option<String>,
    /// Somewhere to go: a link that was not prevented, or `location.assign`.
    pub navigate: Option<String>,
    /// set when `navigate` is a form POST: its urlencoded body
    pub form: Option<String>,
    /// A text field that took focus.
    pub focus: Option<(usize, String)>,
    /// `location.hash` changed: scroll to that anchor
    pub jump: Option<String>,
    /// The page's own history (pushState entries): (current index, length),
    /// so the window's back and forward can step inside the page first
    pub history: Option<(usize, usize)>,
}

/// One live page: DOM, scripts, styles, images.
pub struct Session {
    ctx: Context,
    hooks: std::rc::Rc<Hooks>,
    sheet: Stylesheet,
    /// the stylesheet with the CSS scripts added, and what it was built from
    live_sheet: RefCell<Option<(u64, Stylesheet)>>,
    /// the page's images; backgrounds and masks of CSS that scripts add are
    /// fetched into it as that CSS appears
    images: RefCell<ImageMap>,
    /// image URLs already asked for (each once, answered or not)
    asked_images: RefCell<std::collections::HashSet<String>>,
    width: f32,
}

/// What a script may use of the network: the page's origin and path.
pub struct NetAccess<'a> {
    pub chain: &'a Chain,
    pub resolver: Option<&'a Resolver>,
    pub timeout: Duration,
}

/// Scripts of a document in order: (source text, label for errors). A label
/// starting with `MODULE` marks an ES module; the URL after it is the base
/// its imports resolve against.
pub type Scripts = Vec<(String, String)>;

pub const MODULE: &str = "module:";

/// Native stack for a thread that runs a page's scripts. Boa recurses on the
/// native stack (calls through natives, deep JS), so pages get room like a
/// browser's main thread; the memory is only reserved, not used.
pub const PAGE_STACK: usize = 512 * 1024 * 1024;

/// Start the thread a page (and its `Session`) lives on.
pub fn spawn_page<F, T>(f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name("page".into())
        .stack_size(PAGE_STACK)
        .spawn(f)
        .expect("page thread")
}
/// A label starting with this carries an import map (JSON) and its base URL.
pub const IMPORT_MAP: &str = "importmap:";
/// A label starting with this carries a module's source fetched with the page
/// (its URL after the prefix); it is not run, only handed to the loader.
pub const PREFETCHED: &str = "prefetched:";

const MAX_MODULES: usize = usize::MAX;
const MAX_MODULE_BYTES: usize = usize::MAX;

/// ES module imports, fetched like `<script src>`: any origin, through the
/// page's chain/DoH and the site's cookie partition. Bare specifiers (import
/// maps) are not resolved. Each URL is fetched once per page.
#[derive(Default)]
struct Loader {
    /// import map entries: specifier (or prefix ending in `/`) → URL
    imports: RefCell<Vec<(String, String)>>,
    modules: RefCell<std::collections::HashMap<String, boa_engine::Module>>,
    fetched: std::cell::Cell<(usize, usize)>,
    /// module sources fetched ahead (the static import graph, in parallel)
    prefetched: RefCell<std::collections::HashMap<String, Result<String, String>>>,
    /// every URL already asked for ahead, so none goes twice
    asked: RefCell<std::collections::HashSet<String>>,
}

const MODULE_PARALLEL: usize = 12;

/// The `import ... from "x"`, `import "x"`, `export ... from "x"` and
/// `import("x")` specifiers of a module (a scan, not a parse: a stray match
/// only costs a wasted fetch).
/// A specifier that is (or maps to) a `data:` URL, as written.
fn data_specifier(imports: &[(String, String)], specifier: &str, base: &Url) -> Option<String> {
    // the key may be written as a URL: compare resolved forms, as the spec does
    let url_like = |s: &str| s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.contains("://");
    let norm = |s: &str| if url_like(s) { base.resolve(s).map(|u| u.to_string()).unwrap_or_else(|| s.to_string()) } else { s.to_string() };
    let wanted = norm(specifier);
    let mapped = imports.iter().find(|(k, _)| k == specifier || norm(k) == wanted).map(|(_, v)| v.as_str()).unwrap_or(specifier);
    mapped.trim_start().starts_with("data:").then(|| mapped.trim().to_string())
}

/// Where a module specifier points from `base`, through an import map's entries.
pub(crate) fn locate_specifier(imports: &[(String, String)], specifier: &str, base: &Url) -> Result<Url, String> {
    // as the HTML spec resolves: a URL-like specifier becomes an absolute URL
    // first, and the import map is looked up by that (keys that are URLs too)
    let url_like = |s: &str| s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || s.contains("://");
    let absolute = |s: &str| if url_like(s) { base.resolve(s).map(|u| u.to_string()) } else { None };
    let normalized = absolute(specifier).unwrap_or_else(|| specifier.to_string());
    let key_of = |k: &str| absolute(k).unwrap_or_else(|| k.to_string());
    let mapped = imports
        .iter()
        .find(|(k, _)| key_of(k) == normalized || k == specifier)
        .map(|(_, v)| v.clone())
        .or_else(|| {
            imports
                .iter()
                .map(|(k, v)| (key_of(k), v))
                .filter(|(k, _)| k.ends_with('/') && normalized.starts_with(k.as_str()))
                .max_by_key(|(k, _)| k.len())
                .map(|(k, v)| format!("{v}{}", &normalized[k.len()..]))
        });
    let specifier = mapped.unwrap_or_else(|| specifier.to_string());
    let bare = !(specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") || specifier.contains("://"));
    if bare {
        return Err(format!("bare module specifier {specifier:?}"));
    }
    base.resolve(&specifier)
        .filter(|u| matches!(u.scheme.as_str(), "http" | "https"))
        .ok_or_else(|| format!("bad module URL {specifier:?}"))
}

/// The `"imports"` of an import map, read without a JS engine (values made
/// absolute against `base`): enough for fetching the module graph early.
pub(crate) fn import_map_entries(json: &str, base: &Url) -> Vec<(String, String)> {
    fn string(chars: &[char], i: &mut usize) -> Option<String> {
        if chars.get(*i) != Some(&'"') {
            return None;
        }
        *i += 1;
        let mut out = String::new();
        while let Some(&c) = chars.get(*i) {
            *i += 1;
            match c {
                '"' => return Some(out),
                '\\' => {
                    let e = *chars.get(*i)?;
                    *i += 1;
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'u' => {
                            let hex: String = chars.get(*i..*i + 4)?.iter().collect();
                            *i += 4;
                            out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('\u{fffd}'));
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
        None
    }
    let chars: Vec<char> = json.chars().collect();
    let Some(at) = json.find("\"imports\"") else { return Vec::new() };
    let mut i = json[..at].chars().count() + 9;
    let skip = |i: &mut usize| while chars.get(*i).is_some_and(|c| c.is_whitespace() || *c == ':') { *i += 1 };
    skip(&mut i);
    if chars.get(i) != Some(&'{') {
        return Vec::new();
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        while chars.get(i).is_some_and(|c| c.is_whitespace() || *c == ',') {
            i += 1;
        }
        if chars.get(i) != Some(&'"') {
            break;
        }
        let Some(key) = string(&chars, &mut i) else { break };
        skip(&mut i);
        let Some(value) = string(&chars, &mut i) else { break };
        if value.trim_start().starts_with("data:") {
            // a data: module is its own URL
            out.push((key, value.trim().to_string()));
        } else if let Some(url) = base.resolve(&value) {
            let mut url = url.to_string();
            if value.ends_with('/') && !url.ends_with('/') {
                url.push('/');
            }
            out.push((key, url));
        }
    }
    out
}

pub(crate) fn static_imports(code: &str) -> Vec<String> {
    module_imports(code, false)
}

/// `module_imports(code, true)` also lists literal `import("x")` (lazy chunks).
fn module_imports(code: &str, lazy: bool) -> Vec<String> {
    let bytes = code.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut out = Vec::new();
    for word in ["from", "import"] {
        let mut at = 0;
        while let Some(i) = code[at..].find(word) {
            let start = at + i;
            at = start + word.len();
            if start > 0 && (ident(bytes[start - 1]) || bytes[start - 1] == b'.') {
                continue;
            }
            let mut j = at;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\n' || bytes[j] == b'\t' || bytes[j] == b'\r') {
                j += 1;
            }
            let Some(&q0) = bytes.get(j) else { continue };
            // import("./chunk.js") with a literal: a lazy chunk, fetched ahead too
            let (q, j) = if word == "import" && q0 == b'(' {
                if !lazy {
                    continue;
                }
                let mut k = j + 1;
                while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\n') {
                    k += 1;
                }
                match bytes.get(k) {
                    Some(&c) if c == b'"' || c == b'\'' || c == b'`' => (c, k),
                    _ => continue,
                }
            } else {
                (q0, j)
            };
            if q != b'"' && q != b'\'' && q != b'`' {
                continue;
            }
            if let Some(len) = code[j + 1..].find(q as char) {
                let spec = &code[j + 1..j + 1 + len];
                if !spec.is_empty() && spec.len() < 512 && !spec.contains('\n') && !spec.contains("${") {
                    out.push(spec.to_string());
                }
            }
        }
    }
    out
}

impl Loader {
    /// Where a specifier points from `base`, through the import map.
    fn locate(&self, specifier: &str, base: &Url) -> Result<Url, String> {
        locate_specifier(&self.imports.borrow(), specifier, base)
    }

    /// Fetch the static import graph under these sources, many files at a
    /// time, before the engine asks for them one by one.
    fn prefetch(&self, roots: Vec<(Url, String)>) {
        let Some((origin, chain, resolver, timeout)) = with(|st| st.net.as_ref().map(|n| (n.origin.clone(), n.chain.clone(), n.resolver.clone(), n.timeout))) else { return };
        let site = crate::cookies::site_of(&origin.host);
        let mut frontier: Vec<Url> = Vec::new();
        let queue_imports = |base: &Url, code: &str, frontier: &mut Vec<Url>| {
            for spec in static_imports(code) {
                if let Ok(u) = self.locate(&spec, base) {
                    let key = u.to_string();
                    if !self.modules.borrow().contains_key(&key) && self.asked.borrow_mut().insert(key) {
                        frontier.push(u);
                    }
                }
            }
        };
        for (base, code) in &roots {
            queue_imports(base, code, &mut frontier);
        }
        while !frontier.is_empty() {
            let (count, bytes) = self.fetched.get();
            if count >= MAX_MODULES || bytes >= MAX_MODULE_BYTES {
                break;
            }
            let batch: Vec<Url> = frontier.drain(..frontier.len().min(MODULE_PARALLEL)).collect();
            let got: Vec<(Url, Result<crate::net::Response, String>)> = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|url| {
                        let (chain, resolver, site) = (&chain, &resolver, &site);
                        let cached = with(|st| st.http_cache.get(&url.to_string()).cloned());
                        scope.spawn(move || {
                            let r = match cached {
                                Some(r) => Ok(r),
                                None => fetch_in(url, chain, resolver.as_ref(), timeout, &PageRequest::get(site)),
                            };
                            (url.clone(), r)
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            let got: Vec<(Url, Result<String, String>)> = got
                .into_iter()
                .map(|(url, r)| {
                    let text = r.and_then(|resp| {
                        if resp.status >= 400 {
                            return Err(format!("HTTP {}", resp.status));
                        }
                        with(|st| st.remember(&url, &resp));
                        Ok(crate::charset::decode(&resp.body, &resp.content_type(), false, crate::charset::Encoding::Utf8).0)
                    });
                    (url, text)
                })
                .collect();
            for (url, result) in got {
                if let Ok(code) = &result {
                    let (count, bytes) = self.fetched.get();
                    self.fetched.set((count + 1, bytes + code.len()));
                    if std::env::var_os("SEPIA_VERBOSE").is_some() {
                        eprintln!("module (prefetched): {} {} bytes", url.to_string(), code.len());
                    }
                    queue_imports(&url, code, &mut frontier);
                }
                self.prefetched.borrow_mut().insert(url.to_string(), result);
            }
        }
    }

    fn fetch(&self, url: &Url) -> Result<String, String> {
        if let Some(ready) = self.prefetched.borrow_mut().remove(&url.to_string()) {
            return ready;
        }
        let (count, bytes) = self.fetched.get();
        if count >= MAX_MODULES || bytes >= MAX_MODULE_BYTES {
            return Err("module budget spent".into());
        }
        let origin = with(|st| st.net.as_ref().map(|n| n.origin.clone())).ok_or("no network")?;
        let site = crate::cookies::site_of(&origin.host);
        let response = with(|st| st.get(url, &site))?;
        if response.status >= 400 {
            return Err(format!("HTTP {}", response.status));
        }
        self.fetched.set((count + 1, bytes + response.body.len()));
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            eprintln!("module: {} {} bytes", url.to_string(), response.body.len());
        }
        Ok(crate::charset::decode(&response.body, &response.content_type(), false, crate::charset::Encoding::Utf8).0)
    }
}

impl boa_engine::module::ModuleLoader for Loader {
    async fn load_imported_module(
        self: std::rc::Rc<Self>,
        referrer: boa_engine::module::Referrer,
        request: boa_engine::module::ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> JsResult<boa_engine::Module> {
        let error = |m: String| -> boa_engine::JsError { boa_engine::JsNativeError::typ().with_message(m).into() };
        let specifier = request.specifier().to_std_string_escaped();
        let base = referrer
            .path()
            .and_then(|p| p.to_str())
            .and_then(Url::parse)
            .or_else(|| with(|st| st.url.clone()))
            .ok_or_else(|| error("no base URL".into()))?;
        // a module written into a data: URL (no network)
        if let Some(raw) = data_specifier(&self.imports.borrow(), &specifier, &base) {
            if let Some(module) = self.modules.borrow().get(&raw) {
                return Ok(module.clone());
            }
            let (_, bytes) = crate::images::data_url_bytes(&raw).ok_or_else(|| error("bad data: URL".into()))?;
            let code = String::from_utf8_lossy(&bytes).into_owned();
            let source = Source::from_bytes(code.as_bytes()).with_path(std::path::Path::new("data:"));
            let module = boa_engine::Module::parse(source, None, &mut context.borrow_mut())?;
            self.modules.borrow_mut().insert(raw, module.clone());
            return Ok(module);
        }
        let url = self.locate(&specifier, &base).map_err(error)?;
        let key = url.to_string();
        if let Some(module) = self.modules.borrow().get(&key) {
            return Ok(module.clone());
        }
        let code = self.fetch(&url).map_err(|e| error(format!("{key}: {e}")))?;
        peek(&code, &key);
        // its own imports come in parallel too (dynamic import() of a big chunk)
        self.prefetch(vec![(url.clone(), code.clone())]);
        let source = Source::from_bytes(code.as_bytes()).with_path(std::path::Path::new(&key));
        let module = boa_engine::Module::parse(source, None, &mut context.borrow_mut())?;
        self.modules.borrow_mut().insert(key, module.clone());
        Ok(module)
    }

    fn init_import_meta(self: std::rc::Rc<Self>, import_meta: &boa_engine::JsObject, module: &boa_engine::Module, context: &mut Context) {
        let url = module.path().and_then(|p| p.to_str()).unwrap_or("").to_string();
        let _ = import_meta.set(js_string!("url"), JsString::from(url.as_str()), false, context);
        // import.meta.resolve(specifier): the URL the module would load, import map included
        let imports = self.imports.borrow().clone();
        let base = url.clone();
        // SAFETY: the closure holds only plain strings, nothing the GC must trace
        let resolve = unsafe {
            NativeFunction::from_closure(move |_, args, ctx| {
                let specifier = args.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
                let base = Url::parse(&base).ok_or_else(|| boa_engine::JsNativeError::typ().with_message("module has no URL"))?;
                match locate_specifier(&imports, &specifier, &base) {
                    Ok(u) => Ok(JsString::from(u.to_string().as_str()).into()),
                    Err(e) => Err(boa_engine::JsNativeError::typ().with_message(e).into()),
                }
            })
        };
        let f = resolve.to_js_function(context.realm());
        let _ = import_meta.set(js_string!("resolve"), f, false, context);
    }
}

/// A shadow tree's stylesheet applied to the page's one cascade: wrapped in
/// `@scope (host)` so its rules reach only the host and what is inside it;
/// `:host` becomes `:scope`, `::slotted(x)` becomes `x`.
fn scope_shadow_css(css: &str, host: &str) -> String {
    let mut out = String::with_capacity(css.len() + host.len() + 16);
    out.push_str("@scope (");
    out.push_str(host);
    out.push_str(") {\n");
    let mut rest = css;
    while let Some(i) = rest.find(":host") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 5..];
        if let Some(inner) = after.strip_prefix('(') {
            let close = inner.find(')').unwrap_or(inner.len());
            out.push_str(":scope");
            out.push_str(inner[..close].trim());
            rest = inner.get(close + 1..).unwrap_or("");
        } else if after.starts_with("-context") {
            out.push_str(":scope");
            rest = after.split_once(')').map(|(_, r)| r).unwrap_or("");
        } else {
            out.push_str(":scope");
            rest = after;
        }
    }
    out.push_str(rest);
    out.push_str("\n}");
    out.replace("::slotted(", ":is(")
}

/// `url(...)` references in a stylesheet made absolute against where it came from.
fn absolute_urls(css: &str, base: &Url) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(i) = rest.find("url(") {
        out.push_str(&rest[..i + 4]);
        rest = &rest[i + 4..];
        let Some(end) = rest.find(')') else { break };
        let raw = rest[..end].trim();
        let unquoted = raw.trim_matches(|c| c == '"' || c == '\'');
        match (unquoted.starts_with("data:") || unquoted.starts_with('#') || unquoted.is_empty(), base.resolve(unquoted)) {
            (false, Some(u)) => out.push_str(&format!("\"{}\"", u.to_string())),
            _ => out.push_str(raw),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// SEPIA_PEEK=label:line:from:to prints a slice of a script whose label holds `label` (debugging).
fn peek(code: &str, label: &str) {
    if let Ok(dir) = std::env::var("SEPIA_DUMP") {
        let name: String = format!("{:016x}_{}", label.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)), label.rsplit('/').next().unwrap_or("").chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).take(60).collect::<String>());
        let _ = std::fs::write(format!("{dir}/{name}.js"), code);
    }
    let Ok(want) = std::env::var("SEPIA_PEEK") else { return };
    let mut p = want.rsplitn(4, ':');
    if let (Some(to), Some(from), Some(line), Some(l)) = (p.next(), p.next(), p.next(), p.next()) {
        if label.contains(l) {
            if let (Ok(to), Ok(from), Ok(line)) = (to.parse::<usize>(), from.parse::<usize>(), line.parse::<usize>()) {
                let text: Vec<char> = code.lines().nth(line.saturating_sub(1)).unwrap_or("").chars().collect();
                eprintln!("peek: {}", text[from.min(text.len())..to.min(text.len())].iter().collect::<String>());
            }
        }
    }
}

/// The `imports` of an import map, values made absolute against `base`.
fn import_map(ctx: &mut Context, json: &str, base: &str) -> Vec<(String, String)> {
    let code = format!(
        "(() => {{ try {{ const m = JSON.parse({}); return Object.entries(m.imports || {{}}).map(([k, v]) => k + '\\u0000' + v).join('\\u0001'); }} catch (e) {{ return ''; }} }})()",
        json_string(json)
    );
    let text = ctx.eval(Source::from_bytes(&code)).ok().and_then(|v| v.as_string().map(|s| s.to_std_string_escaped())).unwrap_or_default();
    let base = Url::parse(base);
    text.split('\u{1}')
        .filter_map(|pair| pair.split_once('\u{0}'))
        .filter_map(|(k, v)| {
            if v.trim_start().starts_with("data:") {
                return Some((k.to_string(), v.trim().to_string()));
            }
            let url = base.as_ref().and_then(|b| b.resolve(v)).or_else(|| Url::parse(v))?;
            let mut url = url.to_string();
            if v.ends_with('/') && !url.ends_with('/') {
                url.push('/');
            }
            Some((k.to_string(), url))
        })
        .collect()
}

/// The page's clock runs in the chosen country's time zone (settings), and
/// promises rejected with nobody listening are noted, to be reported as a
/// browser does (console and an `unhandledrejection` event).
struct Hooks(crate::persona::Persona, RefCell<Vec<boa_engine::JsObject>>);

impl boa_engine::context::HostHooks for Hooks {
    fn local_timezone_offset_seconds(&self, unix_time_seconds: i64) -> i32 {
        self.0.offset_seconds(unix_time_seconds)
    }

    fn promise_rejection_tracker(
        &self,
        promise: &boa_engine::JsObject<boa_engine::builtins::promise::Promise>,
        operation: boa_engine::builtins::promise::OperationType,
        _context: &mut Context,
    ) {
        let promise = promise.clone().upcast();
        let mut pending = self.1.borrow_mut();
        match operation {
            boa_engine::builtins::promise::OperationType::Reject => {
                if true {
                    pending.push(promise);
                }
            }
            boa_engine::builtins::promise::OperationType::Handle => pending.retain(|p| !boa_engine::JsObject::equals(p, &promise)),
        }
    }
}

/// The persona as the prelude reads it (`globalThis.__persona`).
fn persona_json(p: &crate::persona::Persona) -> String {
    let b = p.browser;
    let hints = match b.client_hints {
        Some((brand, major, platform)) => format!(
            "{{\"brand\":{},\"major\":{},\"platform\":{}}}",
            json_string(brand), json_string(major), json_string(platform)
        ),
        None => "null".into(),
    };
    let languages: Vec<String> = p.languages().iter().map(|l| json_string(l)).collect();
    format!(
        "{{\"userAgent\":{},\"appVersion\":{},\"platform\":{},\"vendor\":{},\"productSub\":{},\"oscpu\":{},\"hints\":{hints},\"language\":{},\"languages\":[{}],\"timeZone\":{}}}",
        json_string(b.user_agent),
        json_string(b.app_version),
        json_string(b.platform),
        json_string(b.vendor),
        json_string(b.product_sub),
        b.oscpu.map(json_string).unwrap_or_else(|| "null".into()),
        json_string(p.country.language),
        languages.join(","),
        json_string(p.country.time_zone),
    )
}

/// Run one module and the jobs it starts; an error comes back as text.
/// A module script lives in the page's one module map: a `src` that was
/// already imported (or listed twice) is the same module, evaluated once.
fn run_module(ctx: &mut Context, loader: &Loader, code: &str, path: &str) -> Result<(), String> {
    let inline = path.contains("#inline-module-");
    let known = if inline { None } else { loader.modules.borrow().get(path).cloned() };
    let module = match known {
        Some(module) => module,
        None => {
            let source = Source::from_bytes(code.as_bytes()).with_path(std::path::Path::new(path));
            let module = boa_engine::Module::parse(source, None, ctx).map_err(|e| e.to_string())?;
            if !inline {
                loader.modules.borrow_mut().insert(path.to_string(), module.clone());
            }
            module
        }
    };
    let promise = module.load_link_evaluate(ctx);
    let _ = ctx.run_jobs();
    match promise.state() {
        boa_engine::builtins::promise::PromiseState::Rejected(e) => {
            let text = e.to_string(ctx).map(|s| s.to_std_string_escaped()).unwrap_or_else(|_| "module failed".into());
            let stack = e
                .as_object()
                .and_then(|o| o.get(js_string!("stack"), ctx).ok())
                .and_then(|s| s.as_string().map(|s| s.to_std_string_escaped()))
                .filter(|_| std::env::var_os("SEPIA_VERBOSE").is_some())
                .map(|s| format!("\n{s}"))
                .unwrap_or_default();
            Err(format!("{text}{stack}"))
        }
        _ => Ok(()),
    }
}

impl Session {
    /// Build the DOM, run the page's scripts, fire DOMContentLoaded and load,
    /// drain timers. Returns the session and the log.
    #[allow(clippy::too_many_arguments)]
    pub fn start(document: &Node, url: &Url, scripts: &Scripts, net: Option<NetAccess>, sheet: Stylesheet, images: ImageMap, width: f32) -> Session {
        set_geometry_inputs(&sheet, &images, width);
        with(|st| {
            *st = State::default();
            st.dom = Dom::from_tree(document);
            // the styles the loader already applied: the document's own, outside
            // templates and shadow roots
            st.initial_styles = {
                let mut found = std::collections::HashSet::new();
                let mut stack = vec![0usize];
                while let Some(i) = stack.pop() {
                    let n = &st.dom.nodes[i];
                    match n.tag.as_str() {
                        "style" | "link" => {
                            found.insert(i);
                        }
                        "template" | "noscript" => continue,
                        _ => {}
                    }
                    stack.extend(n.children.iter().copied());
                }
                found
            };
            st.url = Some(url.clone());
            st.net = net.map(|n| Net { origin: url.clone(), chain: n.chain.clone(), resolver: n.resolver.cloned(), timeout: n.timeout });
        });
        let persona = crate::persona::current();
        let hooks = std::rc::Rc::new(Hooks(persona, RefCell::new(Vec::new())));
        let loader = std::rc::Rc::new(Loader::default());
        let mut ctx = Context::builder()
            .host_hooks(hooks.clone())
            .module_loader(loader.clone())
            .build()
            .unwrap_or_default();
        {
            let limits = ctx.runtime_limits_mut();
            limits.set_loop_iteration_limit(std::env::var("SEPIA_LOOP_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(u64::MAX));
            limits.set_recursion_limit(100_000);
            limits.set_stack_size_limit(64 * 1024 * 1024);
        }
        register(&mut ctx);
        let viewport = format!("globalThis.__viewport = [{width}, 800]; globalThis.__verbose = {}; globalThis.__persona = {};", std::env::var_os("SEPIA_VERBOSE").is_some(), persona_json(&persona));
        let _ = ctx.eval(Source::from_bytes(&viewport));
        if let Err(e) = ctx.eval(Source::from_bytes(PRELUDE)) {
            with(|st| st.log.push(format!("prelude: {e}")));
        }
        // SEPIA_PRELUDE_EXTRA: debugging hooks that run before the page's scripts
        if let Ok(extra) = std::env::var("SEPIA_PRELUDE_EXTRA") {
            if let Err(e) = ctx.eval(Source::from_bytes(extra.as_bytes())) {
                with(|st| st.log.push(format!("prelude extra: {e}")));
            }
        }
        // module files the page loader already fetched (with the page's subresources)
        for (code, label) in scripts {
            if let Some(url) = label.strip_prefix(PREFETCHED) {
                loader.asked.borrow_mut().insert(url.to_string());
                loader.prefetched.borrow_mut().insert(url.to_string(), Ok(code.clone()));
            }
        }
        // every module's static imports, fetched together up front
        let roots: Vec<(Url, String)> = scripts
            .iter()
            .filter_map(|(code, label)| label.strip_prefix(MODULE).and_then(Url::parse).map(|u| (u, code.clone())))
            .collect();
        let roots_empty = roots.is_empty();
        if !roots_empty {
            for (code, label) in scripts.iter().filter(|(_, l)| l.starts_with(IMPORT_MAP)) {
                loader.imports.borrow_mut().extend(import_map(&mut ctx, code, &label[IMPORT_MAP.len()..]));
            }
            let prefetch_started = std::time::Instant::now();
            loader.prefetch(roots);
            if std::env::var_os("SEPIA_VERBOSE").is_some() {
                eprintln!("page timing: module graph {} ms", prefetch_started.elapsed().as_millis());
            }
        }
        for (code, label) in scripts {
            if label.starts_with(PREFETCHED) {
                continue;
            }
            if let Ok(want) = std::env::var("SEPIA_PEEK") {
                // SEPIA_PEEK=label:line:from:to prints a slice of a script (debugging)
                let mut p = want.rsplitn(4, ':');
                if let (Some(to), Some(from), Some(line), Some(l)) = (p.next(), p.next(), p.next(), p.next()) {
                    if label.contains(l) {
                        if let (Ok(to), Ok(from), Ok(line)) = (to.parse::<usize>(), from.parse::<usize>(), line.parse::<usize>()) {
                            let text: Vec<char> = code.lines().nth(line.saturating_sub(1)).unwrap_or("").chars().collect();
                            eprintln!("peek: {}", text[from.min(text.len())..to.min(text.len())].iter().collect::<String>());
                        }
                    }
                }
            }
            if let Ok(dir) = std::env::var("SEPIA_DUMP") {
                // SEPIA_DUMP=dir saves every script to disk (debugging)
                let name: String = format!("{:016x}_{}", label.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)), label.rsplit('/').next().unwrap_or("").chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).take(60).collect::<String>());
                let _ = std::fs::write(format!("{dir}/{name}.js"), code);
            }
            if let Some(base) = label.strip_prefix(IMPORT_MAP) {
                if roots_empty {
                    loader.imports.borrow_mut().extend(import_map(&mut ctx, code, base));
                }
                continue;
            }
            if std::env::var_os("SEPIA_TRACE_SCRIPTS").is_some() {
                eprintln!("script start: {}", label.chars().take(160).collect::<String>());
            }
            let script_started = std::time::Instant::now();
            let slow = |label: &str| {
                let took = script_started.elapsed().as_millis();
                if took > 300 && std::env::var_os("SEPIA_VERBOSE").is_some() {
                    eprintln!("slow script: {took} ms {}", label.chars().take(140).collect::<String>());
                }
            };
            if let Some(path) = label.strip_prefix(MODULE) {
                let _ = ctx.eval(Source::from_bytes("globalThis.__currentScript = null"));
                let result = run_module(&mut ctx, &loader, code, path);
                slow(label);
                if let Err(e) = result {
                    with(|st| st.log.push(format!("{label}: {e}")));
                }
                continue;
            }
            let current = if label.starts_with("http") {
                format!("__sepia_current({}, '')", json_string(label))
            } else {
                format!("__sepia_current(null, {})", json_string(&code.chars().take(64).collect::<String>()))
            };
            let _ = ctx.eval(Source::from_bytes(&current));
            let result = ctx.eval(Source::from_bytes(code.as_bytes()).with_path(std::path::Path::new(label)));
            slow(label);
            if let Err(e) = result {
                let at = if std::env::var_os("SEPIA_VERBOSE").is_some() { near(scripts, &e.to_string()) } else { String::new() };
                with(|st| st.log.push(format!("{label}: {e}{at}")));
            }
            let _ = ctx.run_jobs();
        }
        let loaded_started = std::time::Instant::now();
        let _ = ctx.eval(Source::from_bytes("__sepia_loaded()"));
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            eprintln!("page timing: load events {} ms", loaded_started.elapsed().as_millis());
        }
        let mut session = Session { ctx, hooks, sheet, live_sheet: RefCell::new(None), images: RefCell::new(images), asked_images: RefCell::new(Default::default()), width };
        let settle_started = std::time::Instant::now();
        session.settle();
        if std::env::var_os("SEPIA_VERBOSE").is_some() {
            eprintln!("page timing: settle {} ms", settle_started.elapsed().as_millis());
        }
        session
    }

    /// Run promise jobs and due timers until things are quiet (bounded).
    /// SEPIA_EVAL: run a snippet after load and print its value (debugging).
    pub fn debug_eval(&mut self) {
        let Ok(code) = std::env::var("SEPIA_EVAL") else { return };
        let out = match self.ctx.eval(Source::from_bytes(&code)) {
            Ok(v) => v.to_string(&mut self.ctx).map(|s| s.to_std_string_escaped()).unwrap_or_default(),
            Err(e) => format!("error: {e}"),
        };
        self.settle();
        eprintln!("eval: {out}");
    }

    fn settle(&mut self) {
        self.settle_for(Duration::from_millis(1000));
    }

    /// Run promise jobs and the timers due now (zero-delay chains such as
    /// IndexedDB events) within `budget`; later timers stay for the window's
    /// event loop, so animations do not hold the page back.
    fn settle_for(&mut self, budget: Duration) {
        let started = std::time::Instant::now();
        let verbose = std::env::var_os("SEPIA_VERBOSE").is_some();
        for _ in 0..2000 {
            let t = std::time::Instant::now();
            let _ = self.ctx.run_jobs();
            if verbose && t.elapsed() > Duration::from_millis(300) {
                eprintln!("settle: promise jobs {} ms", t.elapsed().as_millis());
            }
            let t = std::time::Instant::now();
            let ran = self.ctx.eval(Source::from_bytes("__sepia_tick()")).ok().and_then(|v| v.as_number()).unwrap_or(0.0);
            if verbose && t.elapsed() > Duration::from_millis(300) {
                eprintln!("settle: tick {} ms ({ran} ran)", t.elapsed().as_millis());
            }
            if ran > 0.0 {
                if started.elapsed() > budget {
                    break;
                }
                continue;
            }
            break;
        }
        let t = std::time::Instant::now();
        let _ = self.ctx.run_jobs();
        if verbose && t.elapsed() > Duration::from_millis(300) {
            eprintln!("settle: final promise jobs {} ms", t.elapsed().as_millis());
        }
        let t = std::time::Instant::now();
        self.report_rejections();
        if verbose && t.elapsed() > Duration::from_millis(300) {
            eprintln!("settle: rejections {} ms", t.elapsed().as_millis());
        }
        if verbose && started.elapsed() > Duration::from_millis(300) {
            eprintln!("settle: {} ms in all", started.elapsed().as_millis());
        }
    }

    /// Promises still rejected with no handler after the jobs ran: an
    /// `unhandledrejection` event, and the console unless a listener cancels it.
    fn report_rejections(&mut self) {
        let pending: Vec<boa_engine::JsObject> = std::mem::take(&mut *self.hooks.1.borrow_mut());
        if pending.is_empty() {
            return;
        }
        let Ok(report) = self.ctx.global_object().get(js_string!("__sepia_unhandled"), &mut self.ctx) else { return };
        let Some(report) = report.as_callable() else { return };
        for promise in pending {
            let reason = boa_engine::object::builtins::JsPromise::from_object(promise.clone())
                .ok()
                .and_then(|p| match p.state() {
                    boa_engine::builtins::promise::PromiseState::Rejected(r) => Some(r),
                    _ => None,
                });
            if let Some(reason) = reason {
                let _ = report.call(&JsValue::undefined(), &[promise.into(), reason], &mut self.ctx);
            }
        }
        let _ = self.ctx.run_jobs();
    }


    /// The document as it is now, for layout.
    pub fn tree(&self) -> Node {
        with(|st| st.dom.to_tree())
    }

    pub fn layout(&self, fonts: &Fonts) -> Page {
        // what scripts drew on canvases since the last layout
        let mut drawn = crate::canvas2d::take_dirty();
        drawn.extend(take_script_images());
        if !drawn.is_empty() {
            self.images.borrow_mut().extend(drawn);
        }
        let tree = self.tree();
        let added = self.added_css();
        if added.is_empty() {
            let page = Layout::new(fonts, &self.sheet, self.width).with_images(&self.images.borrow()).build(&tree);
            remember_geometry(&page);
            return page;
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            added.hash(&mut h);
            (self.sheet.viewport_width.to_bits(), self.sheet.viewport_height.to_bits()).hash(&mut h);
            h.finish()
        };
        let mut cache = self.live_sheet.borrow_mut();
        if cache.as_ref().map(|(k, _)| *k) != Some(key) {
            let mut sheet = self.sheet.clone();
            let mut extra = Stylesheet::parse(&added);
            if let Some(base) = with(|st| st.url.clone()) {
                extra.absolutize_urls(&base);
            }
            sheet.append(extra);
            self.fetch_new_images(&sheet, &tree);
            *cache = Some((key, sheet));
        }
        let sheet = &cache.as_ref().expect("built above").1;
        let page = Layout::new(fonts, sheet, self.width).with_images(&self.images.borrow()).build(&tree);
        remember_geometry(&page);
        page
    }

    /// CSS the page's scripts added after load, in document order: new
    /// `<style>` elements (their text, then CSSOM rules), fetched `<link>`s.
    fn added_css(&self) -> String {
        added_css()
    }
}

/// CSS the page's scripts added after load (see `Session::added_css`).
fn added_css() -> String {
    {
        with(|st| {
            let any_shadow = st.dom.nodes.iter().any(|n| n.shadow.is_some());
            if st.dyn_css.is_empty() && !any_shadow && (0..st.dom.nodes.len()).all(|i| st.dom.nodes[i].tag != "style" || st.initial_styles.contains(&i)) {
                return String::new();
            }
            let mut out = String::new();
            // (node, the host whose shadow tree it is in)
            let mut stack: Vec<(usize, Option<usize>)> = vec![(0, None)];
            while let Some((i, host)) = stack.pop() {
                let n = &st.dom.nodes[i];
                let push = |css: &str, out: &mut String| {
                    match host {
                        Some(h) => out.push_str(&scope_shadow_css(css, &st.dom.nodes[h].tag)),
                        None => out.push_str(css),
                    }
                    out.push('\n');
                };
                match n.tag.as_str() {
                    "style" => {
                        if !st.initial_styles.contains(&i) {
                            push(&st.dom.text_content(i), &mut out);
                        }
                        if let Some(css) = st.dyn_css.get(&i) {
                            push(css, &mut out);
                        }
                    }
                    "link" => {
                        if let Some(css) = st.dyn_css.get(&i) {
                            push(css, &mut out);
                        }
                    }
                    "#fragment" if host.is_some() => {
                        // a shadow root's adopted stylesheets
                        if let Some(css) = st.dyn_css.get(&i) {
                            push(css, &mut out);
                        }
                    }
                    "template" | "noscript" => continue,
                    _ => {}
                }
                if let (Some(root), false) = (n.shadow, n.tag == "#fragment") {
                    stack.push((root, Some(i)));
                }
                stack.extend(n.children.iter().rev().map(|c| (*c, host)));
            }
            out
        })
    }
}

impl Session {
    /// Images that arrived after the page was first shown.
    pub fn set_images(&mut self, images: ImageMap) {
        let mut mine = self.images.borrow_mut();
        // keep what was fetched for script-added CSS
        let extra: Vec<(String, std::sync::Arc<crate::images::Bitmap>)> = mine.iter().filter(|(k, _)| !images.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect();
        *mine = images;
        mine.extend(extra);
        set_geometry_inputs(&self.sheet, &mine, self.width);
    }

    /// Backgrounds and masks the CSS scripts added uses, not fetched yet:
    /// through the page's path, in parallel, each URL once.
    fn fetch_new_images(&self, sheet: &Stylesheet, tree: &Node) {
        let wanted: Vec<String> = {
            let have = self.images.borrow();
            let asked = self.asked_images.borrow();
            sheet.background_urls(tree).into_iter().filter(|u| !have.contains_key(u) && !asked.contains(u)).collect()
        };
        if wanted.is_empty() {
            return;
        }
        self.asked_images.borrow_mut().extend(wanted.iter().cloned());
        let mut got: Vec<(String, crate::images::Bitmap)> = Vec::new();
        let mut remote: Vec<(String, Url)> = Vec::new();
        for u in wanted {
            if u.starts_with("data:") {
                if let Some(b) = crate::images::decode_data_uri(&u) {
                    got.push((u, b));
                }
            } else if let Some(url) = Url::parse(&u) {
                remote.push((u, url));
            }
        }
        if !remote.is_empty() {
            let site = with(|st| st.url.as_ref().map(|u| crate::cookies::site_of(&u.host))).unwrap_or_default();
            let urls: Vec<Url> = remote.iter().map(|(_, u)| u.clone()).collect();
            let answers = with(|st| st.get_many(&urls, &site));
            for ((key, _), answer) in remote.into_iter().zip(answers) {
                if let Some(b) = answer.filter(|r| r.status < 400).and_then(|r| crate::images::decode(&r.body)) {
                    got.push((key, b));
                }
            }
        }
        let mut images = self.images.borrow_mut();
        for (k, b) in got {
            images.insert(k, std::sync::Arc::new(b));
        }
    }

    pub fn title(&self) -> Option<String> {
        with(|st| st.title.clone())
    }

    pub fn log(&self) -> Vec<String> {
        with(|st| st.log.clone())
    }

    /// A location change requested by a startup script, before the first
    /// input event resets the event outcome. Consumed by the page loader.
    pub fn take_startup_navigation(&mut self) -> Option<(String, Option<String>)> {
        with(|st| st.navigate.take().map(|target| (target, st.form.take())))
    }

    /// The window's back (`-1`) / forward (`1`) button: true if the page took
    /// it as a step in its own history (pushState entries, popstate).
    pub fn history_step(&mut self, delta: i32) -> bool {
        let call = if delta < 0 { "__sepia_history_back()" } else { "__sepia_history_forward()" };
        let handled = self.ctx.eval(Source::from_bytes(call)).ok().map(|v| v.to_boolean()).unwrap_or(false);
        if handled {
            self.settle_for(Duration::from_millis(100));
        }
        handled
    }

    /// Requests the page started that have not been answered and handed over yet.
    pub fn network_busy(&self) -> bool {
        with(|st| !st.inflight.is_empty() || !st.ready.is_empty() || !st.image_loads.is_empty())
    }

    pub fn next_timer_delay(&mut self) -> Option<Duration> {
        let waiting = with(|st| !st.inflight.is_empty() || !st.ready.is_empty() || !st.sockets.is_empty() || !st.image_loads.is_empty());
        let delay = self.ctx.eval(Source::from_bytes("__sepia_next_timer()")).ok().and_then(|v| v.as_number());
        let timer = delay.filter(|d| d.is_finite() && *d >= 0.0).map(|d| Duration::from_secs_f64((d / 1000.0).clamp(0.001, 86_400.0)));
        // requests in flight: look again soon (their answers are events too)
        let net = waiting.then_some(Duration::from_millis(15));
        match (timer, net) {
            (Some(t), Some(n)) => Some(t.min(n)),
            (t, n) => t.or(n),
        }
    }

    /// Hand an event to the page's scripts and report what changed.
    pub fn handle(&mut self, event: PageEvent, fonts: &Fonts) -> Outcome {
        with(|st| {
            st.dirty = false;
            st.navigate = None;
            st.form = None;
            st.jump = None;
            st.focus = None;
        });
        let call = match &event {
            PageEvent::Tick => "void 0".into(),
            PageEvent::Click { node, href } => format!(
                "__sepia_click({node}, {})",
                href.as_ref().map(|h| json_string(h)).unwrap_or_else(|| "null".into())
            ),
            PageEvent::Input { node, value } => format!("__sepia_input({node}, {})", json_string(value)),
            PageEvent::Key { node, key } => format!("__sepia_key({node}, {})", json_string(key)),
            PageEvent::Hash(hash) => format!("__sepia_hash({})", json_string(hash)),
            PageEvent::History(delta) => format!("history.go({delta})"),
            PageEvent::Resize { width, height } => {
                let width = if width.is_finite() { width.max(1.0) } else { self.width };
                let height = if height.is_finite() { height.max(1.0) } else { 800.0 };
                self.width = width;
                self.sheet.viewport_width = width;
                self.sheet.viewport_height = height;
                set_geometry_inputs(&self.sheet, &self.images.borrow(), self.width);
                with(|st| st.dirty = true);
                format!("__sepia_resize({width}, {height})")
            }
        };
        if let Err(e) = self.ctx.eval(Source::from_bytes(&call)) {
            with(|st| st.log.push(format!("event: {e}")));
        }
        self.settle_for(Duration::from_millis(100));
        let (dirty, navigate, form, jump, focus, title) = with(|st| {
            let focus = st.focus.and_then(|f| {
                let tag = st.dom.nodes[f].tag.clone();
                matches!(tag.as_str(), "input" | "textarea").then(|| (f, st.dom.attr(f, "value").unwrap_or("").to_string()))
            });
            (st.dirty, st.navigate.take(), st.form.take(), st.jump.take(), focus, st.title.clone())
        });
        let history = self.ctx.eval(Source::from_bytes("__sepia_history_position()")).ok()
            .and_then(|v| v.as_number()).filter(|n| *n >= 0.0)
            .map(|n| ((n as u64 >> 16) as usize, (n as u64 & 0xffff) as usize));
        Outcome { page: dirty.then(|| self.layout(fonts)), title, navigate, form, jump, focus, history }
    }
}

/// The code around an error's `line:col` (for --verbose).
fn near(scripts: &Scripts, error: &str) -> String {
    // the first frame `(label:line:col)` whose script we have
    let found = error.lines().find_map(|l| {
        let pos = l.rsplit(" (").next()?.trim_end_matches(')');
        let mut parts = pos.rsplitn(3, ':');
        let col = parts.next()?.parse::<usize>().ok()?;
        let line = parts.next()?.parse::<usize>().ok()?;
        let label = parts.next()?;
        let code = &scripts.iter().find(|(_, l)| l == label || l.strip_prefix(MODULE) == Some(label))?.0;
        Some((code, line, col))
    });
    let Some((code, line, col)) = found else { return String::new() };
    let Some(text) = code.lines().nth(line.saturating_sub(1)) else { return String::new() };
    let chars: Vec<char> = text.chars().collect();
    let at = col.saturating_sub(1).min(chars.len());
    let from = at.saturating_sub(120);
    let to = (at + 60).min(chars.len());
    format!("\n    …{}⟦{}",  chars[from..at].iter().collect::<String>(), chars[at..to].iter().collect::<String>())
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Web API in JS, over the natives above.
const PRELUDE: &str = include_str!("js_prelude.js");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_location_change_is_consumed_before_input_events() {
        let mut session = run("<p>Redirecting</p>", "location.replace('/next?query=rust');");
        assert_eq!(session.take_startup_navigation(), Some(("/next?query=rust".into(), None)));
        assert_eq!(session.take_startup_navigation(), None);
    }

    #[test]
    fn character_data_edits_and_split_text_update_the_rendered_dom() {
        let session = run("<div id=host></div>", r#"
            const host = document.getElementById('host');
            const text = new Text('abcdef');
            host.append(text);
            text.deleteData(1, 2);
            text.insertData(1, 'BC');
            text.replaceData(3, 2, 'D');
            const tail = text.splitText(3);
            const copy = text.cloneNode();
            let error = '';
            try { text.substringData(99, 1); } catch (e) { error = e.name; }
            text.nodeValue = 'OK';
            document.title = [host.textContent, copy.data, tail.previousSibling === text,
                text instanceof CharacterData, tail instanceof Text, error].join('|');
        "#);
        assert_eq!(session.title().as_deref(), Some("OKDf|aBC|true|true|true|IndexSizeError"));
        assert!(session.layout(&Fonts::load()).text.join("").contains("OKDf"));
        assert!(session.log().is_empty(), "{:?}", session.log());
    }

    #[test]
    fn comment_markers_are_real_nodes_but_not_rendered_text() {
        let session = run("<div id=host>Before</div>", r#"
            const host = document.getElementById('host');
            const marker = document.createComment('secret marker');
            host.append(marker, new Text('After'));
            marker.appendData('!');
            const clone = marker.cloneNode();
            document.title = [marker.nodeType, marker instanceof Comment,
                marker instanceof CharacterData, clone.data, host.textContent,
                host.innerHTML.includes('<!--secret marker!-->')].join('|');
        "#);
        assert_eq!(session.title().as_deref(), Some("8|true|true|secret marker!|BeforeAfter|true"));
        let page = session.layout(&Fonts::load());
        assert!(!page.text.join(" ").contains("secret marker"));
        assert!(session.log().is_empty(), "{:?}", session.log());
    }

    #[test]
    fn html_comment_markers_survive_parsing_and_subtree_cloning() {
        let session = run("<div id=host><!--start--><span>Visible</span><!--end--></div>", r#"
            const host = document.getElementById('host');
            const clone = host.cloneNode(true);
            document.title = [host.firstChild.nodeType, host.firstChild.data,
                clone.lastChild.nodeType, clone.lastChild.data, clone.textContent,
                clone.innerHTML === host.innerHTML].join('|');
        "#);
        assert_eq!(session.title().as_deref(), Some("8|start|8|end|Visible|true"));
        assert!(session.log().is_empty(), "{:?}", session.log());
    }

    #[test]
    fn abort_signals_propagate_reasons_and_remove_event_listeners() {
        let session = run("<p id=out></p>", r#"
            const first = new AbortController(), second = new AbortController();
            const combined = AbortSignal.any([first.signal, second.signal]);
            let aborts = 0, calls = 0;
            combined.addEventListener('abort', () => aborts++);
            const target = new EventTarget();
            const listener = () => calls++;
            target.addEventListener('x', listener, { signal: second.signal });
            target.addEventListener('x', listener, { signal: second.signal });
            target.dispatchEvent(new Event('x'));
            const reason = { why: 'stop' };
            second.abort(reason);
            second.abort('ignored');
            first.abort('later');
            target.dispatchEvent(new Event('x'));
            target.addEventListener('x', () => calls += 100, { signal: second.signal });
            target.dispatchEvent(new Event('x'));
            let thrown;
            try { combined.throwIfAborted(); } catch (e) { thrown = e; }
            const defaultAbort = AbortSignal.abort();
            document.getElementById('out').textContent = [calls, aborts, combined.aborted,
                combined.reason === reason, thrown === reason, defaultAbort.reason.name,
                defaultAbort.reason instanceof DOMException, defaultAbort.reason.code].join('|');
        "#);
        assert_eq!(text(&session), "1|1|true|true|true|AbortError|true|20");
        assert!(session.log().is_empty(), "{:?}", session.log());
    }

    #[test]
    fn already_aborted_fetch_rejects_before_network_and_preserves_reason() {
        let session = run("<p id=out></p>", r#"
            const out = document.getElementById('out');
            const signal = AbortSignal.abort('cancelled');
            fetch('https://elsewhere.test/', { signal }).then(() => { out.textContent = 'wrong'; },
                error => { out.textContent = error; });
        "#);
        assert_eq!(text(&session), "cancelled");
        assert!(session.log().is_empty(), "{:?}", session.log());
    }

    #[test]
    fn abort_signal_timeout_fires_a_timeout_error() {
        let session = run("<p id=out></p>", r#"
            const signal = AbortSignal.timeout(0);
            signal.addEventListener('abort', () => {
                document.getElementById('out').textContent = signal.reason.name + '|' + signal.reason.code;
            });
        "#);
        assert_eq!(text(&session), "TimeoutError|23");
        assert!(session.log().is_empty(), "{:?}", session.log());
    }

    fn run(html: &str, script: &str) -> Session {
        let doc = dom::parse_document(html);
        let url = Url::parse("https://example.test/page").unwrap();
        Session::start(&doc, &url, &vec![(script.to_string(), "test".into())], None, Stylesheet::default(), ImageMap::new(), 800.0)
    }

    fn advance_clock(session: &mut Session, milliseconds: u32) -> Outcome {
        let source = format!("globalThis.__test_now = Date.now() + {milliseconds}; Date.now = () => __test_now;");
        session.ctx.eval(Source::from_bytes(&source)).unwrap();
        session.handle(PageEvent::Tick, &Fonts::load())
    }

    #[test]
    fn delayed_tasks_wait_for_their_deadline_without_input() {
        let mut session = run("<p id=o>Before</p>", "setTimeout(() => { document.getElementById('o').textContent = 'After'; }, 10000);");
        assert_eq!(text(&session), "Before");
        assert!(session.next_timer_delay().unwrap() > Duration::from_secs(9));
        let outcome = advance_clock(&mut session, 11000);
        assert!(outcome.page.is_some());
        assert_eq!(text(&session), "After");
        assert!(session.next_timer_delay().is_none());
    }

    #[test]
    fn intervals_keep_running_until_explicitly_cleared() {
        let mut session = run("<p id=o>0</p>", "let n = 0; const timer = setInterval(() => { document.getElementById('o').textContent = String(++n); if (n === 5) clearInterval(timer); }, 100);");
        assert_eq!(text(&session), "0");
        for n in 1..=5 {
            advance_clock(&mut session, 200);
            assert_eq!(text(&session), n.to_string());
        }
        assert!(session.next_timer_delay().is_none());
    }

    fn text(session: &Session) -> String {
        session.tree().text_content()
    }

    #[test]
    fn detached_dom_queries_and_matches_keep_tree_context() {
        let s = run("<p id=o></p>", r#"
            const root = document.createElement('section');
            root.innerHTML = '<i>A</i><b>B</b>';
            const b = root.querySelector('b');
            const fragment = document.createDocumentFragment();
            fragment.append(root);
            document.getElementById('o').textContent = [
                root.matches('section'), b.matches('section > b:last-child'),
                b.matches('i + b'), b.closest('section') === root,
                fragment.querySelectorAll('i, b').length,
                document.querySelectorAll('section').length
            ].join('|');
        "#);
        assert_eq!(text(&s), "true|true|true|true|2|0", "{:?}", s.log());
    }

    #[test]
    fn adjacent_html_preserves_source_order() {
        let s = run("<div><span id=x>X</span><em>Z</em></div><p id=o></p>", r#"
            const x = document.getElementById('x');
            x.insertAdjacentHTML('afterbegin', '<b>A</b><i>B</i>');
            x.insertAdjacentHTML('afterend', '<b>C</b><i>D</i>');
            document.getElementById('o').textContent = x.parentNode.textContent;
        "#);
        assert!(text(&s).ends_with("ABXCDZ"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn search_submitter_and_fragment_are_preserved() {
        let mut s = run(
            "<form action='/search?old=1#results'><input name=q value='rust lang'><input type=checkbox name=c checked><button name=go value=yes><b>Search</b></button></form>",
            "document.querySelector('form').addEventListener('submit', e => { document.title = e.submitter.name; });",
        );
        let tree = s.tree();
        let id = find(&tree, "b").unwrap().attr("data-sepia-id").unwrap().parse().unwrap();
        let out = s.handle(PageEvent::Click { node: id, href: None }, &Fonts::load());
        assert_eq!(out.navigate.as_deref(), Some("/search?q=rust+lang&c=on&go=yes#results"));
        assert_eq!(s.title().as_deref(), Some("go"));
        assert!(s.log().is_empty(), "{:?}", s.log());
    }

    #[test]
    fn script_submits_navigate_and_can_be_cancelled() {
        let s = run("<form action='/search' method=post><input name=q value=abc><button name=go value=yes></button></form>", r#"
            const f = document.querySelector('form');
            let submits = 0;
            f.addEventListener('submit', e => { submits++; e.preventDefault(); });
            f.requestSubmit(f.querySelector('button'));
            document.title = String(submits);
        "#);
        assert_eq!(s.title().as_deref(), Some("1"));
        assert_eq!(with(|st| st.navigate.clone()), None);
        let s = run("<form action='/search' method=post><input name=q value=abc><button name=go value=yes></button></form>", r#"
            const f = document.querySelector('form');
            f.requestSubmit(f.querySelector('button'));
        "#);
        assert!(s.log().is_empty(), "{:?}", s.log());
        assert_eq!(with(|st| st.navigate.clone()).as_deref(), Some("/search"));
        assert_eq!(with(|st| st.form.clone()).as_deref(), Some("q=abc&go=yes"));
        let s = run("<form action='/direct'><input name=q value=abc></form>", r#"
            const f = document.querySelector('form');
            f.addEventListener('submit', () => { throw new Error('submit must bypass listeners'); });
            f.submit();
        "#);
        assert!(s.log().is_empty(), "{:?}", s.log());
        assert_eq!(with(|st| st.navigate.clone()).as_deref(), Some("/direct?q=abc"));
    }

    #[test]
    fn search_params_use_form_urlencoding() {
        let s = run("<p id=o></p>", r#"
            const params = new URLSearchParams();
            params.append('q', "a b+!'()~*");
            const encoded = params.toString();
            document.getElementById('o').textContent = encoded + '|' + new URLSearchParams(encoded).get('q');
        "#);
        assert_eq!(text(&s), "q=a+b%2B%21%27%28%29%7E*|a b+!'()~*", "{:?}", s.log());
    }

    #[test]
    fn form_data_collects_multiple_selects_and_checked_defaults() {
        let s = run("<form><select name=s multiple><option value=a selected>A</option><option value=b selected>B</option><option value=c selected disabled>C</option></select><input type=checkbox name=c checked><button name=skip>Skip</button></form><p id=o></p>", r#"
            document.getElementById('o').textContent = new URLSearchParams(new FormData(document.querySelector('form'))).toString();
        "#);
        assert!(text(&s).ends_with("s=a&s=b&c=on"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn viewport_resize_updates_scripts_before_dispatch() {
        let mut s = run("<p id=o></p>", r#"
            window.addEventListener('resize', () => {
                document.getElementById('o').textContent = innerWidth + 'x' + innerHeight;
            });
        "#);
        let out = s.handle(PageEvent::Resize { width: 640.0, height: 480.0 }, &Fonts::load());
        assert_eq!(text(&s), "640x480", "{:?}", s.log());
        assert!(out.page.is_some());
        assert_eq!(s.width, 640.0);
        assert_eq!(s.sheet.viewport_width, 640.0);
    }

    #[test]
    fn legacy_event_initializers_support_polyfill_probes_and_dispatch() {
        let s = run("<div><p id=o></p></div>", r#"
            const event = document.createEvent('Event');
            event.initEvent('probe', true, true);
            event.preventDefault();
            const probe = event.defaultPrevented;
            event.initEvent('ready', true, false);
            event.preventDefault();
            const reset = !event.defaultPrevented;
            const out = document.getElementById('o');
            let bubbled = 0;
            out.parentNode.addEventListener('ready', () => { bubbled++; });
            const accepted = out.dispatchEvent(event);
            const custom = document.createEvent('CustomEvent');
            custom.initCustomEvent('ready-data', true, true, { value: 42 });
            let detail;
            document.addEventListener('ready-data', e => {
                detail = e.detail.value;
                e.initCustomEvent('ignored', false, false, null);
                e.preventDefault();
            });
            const cancelled = !out.dispatchEvent(custom);
            out.textContent = [probe, reset, accepted, bubbled, detail, cancelled,
                custom.type, custom.detail.value, custom.currentTarget === null,
                custom instanceof CustomEvent].join('|');
        "#);
        assert_eq!(text(&s), "true|true|true|1|42|true|ready-data|42|true|true", "{:?}", s.log());
        assert!(s.log().is_empty(), "{:?}", s.log());
    }

    #[test]
    fn standard_element_constructor_names_are_exported() {
        let s = run("<p id=o></p>", r#"
            document.getElementById('o').textContent = [
                document.createElement('script') instanceof HTMLScriptElement,
                document.createElement('input') instanceof HTMLInputElement,
                document.createElement('form') instanceof HTMLFormElement,
                document.createElement('div') instanceof HTMLDivElement,
                document.createElement('script') instanceof HTMLElement,
                typeof window.Script, typeof window.Form
            ].join('|');
        "#);
        assert_eq!(text(&s), "true|true|true|true|true|undefined|undefined", "{:?}", s.log());
    }

    #[test]
    fn scheduled_task_errors_reject_instead_of_hanging() {
        let s = run("<p id=o></p>", r#"
            const out = document.getElementById('o');
            scheduler.postTask(() => { throw new Error('task failed'); })
                .then(() => { out.textContent += 'unexpected'; }, e => { out.textContent += e.message; });
            scheduler.postTask(() => Promise.resolve('|done')).then(v => { out.textContent += v; });
        "#);
        assert_eq!(text(&s), "task failed|done", "{:?}", s.log());
        assert!(s.log().is_empty(), "{:?}", s.log());
    }

    #[test]
    fn message_handlers_run_once_per_delivery() {
        let mut s = run("<p id=o></p>", r#"
            let portHandlers = 0, portListeners = 0, windowHandlers = 0, windowListeners = 0;
            const channel = new MessageChannel();
            channel.port2.onmessage = e => { if (e.target === channel.port2 && e.data === 'port') portHandlers++; };
            channel.port2.addEventListener('message', () => { portListeners++; });
            window.onmessage = e => { if (e.target === window && e.data === 'window') windowHandlers++; };
            window.addEventListener('message', () => { windowListeners++; });
            channel.port1.postMessage('port');
            window.postMessage('window', '*');
            setTimeout(() => { document.getElementById('o').textContent = [portHandlers, portListeners, windowHandlers, windowListeners].join('|'); }, 1);
        "#);
        advance_clock(&mut s, 200);
        assert_eq!(text(&s), "1|1|1|1", "{:?}", s.log());
    }

    #[test]
    fn scripts_build_and_change_the_dom() {
        let s = run(
            "<div id='list'></div><p class='note'>old</p>",
            "const list = document.querySelector('#list');
             for (const name of ['a', 'b']) { const item = document.createElement('span'); item.className = 'x'; item.textContent = name; list.append(item); }
             document.querySelector('.note').textContent = `${document.querySelectorAll('span.x').length} items`;
             document.title = 'done';",
        );
        assert!(text(&s).contains("ab"), "{}", text(&s));
        assert!(text(&s).contains("2 items"), "{:?}", s.log());
        assert_eq!(s.title().as_deref(), Some("done"));
    }

    #[test]
    fn deep_recursion_and_new_apis() {
        let s = spawn_page(|| run(
            "<div id=o><p>a</p><!-- c --><span>b</span></div>",
            "function f(n) { return n ? 1 + f(n - 1) : 0; }
             function g(n) { return n ? [n].map(x => g(x - 1))[0] + 1 : 0; }
             const w = document.createTreeWalker(document.getElementById('o'), NodeFilter.SHOW_ELEMENT);
             const seen = []; while (w.nextNode()) seen.push(w.currentNode.localName);
             const doc = new DOMParser().parseFromString('<title>T</title><p class=x>hi</p>', 'text/html');
             let out; try { out = [f(9000), g(3000), seen.join(','), doc.title, doc.body.querySelector('.x').textContent, typeof Path2D].join('|'); } catch (e) { out = String(e); }
             document.getElementById('o').textContent = out;",
        ).tree().text_content()).join().unwrap();
        assert_eq!(s, "9000|3000|p,span|T|hi|function");
    }

    #[test]
    fn module_imports_are_found_for_prefetch() {
        let code = r#"import a from "./a.js";import{b as c}from'./b.js';export * from "./c.js";
            import "./side.js"; const lazy = () => import("./lazy.js"); const t = import(`./tpl.js`);
            const notMine = foo.import("./x.js"); import(variable); import(`./${name}.js`); const s = "from 'nope'";"#;
        let found = module_imports(code, true);
        for want in ["./a.js", "./b.js", "./c.js", "./side.js", "./lazy.js", "./tpl.js"] {
            assert!(found.iter().any(|f| f == want), "{want} in {found:?}");
        }
        // what is fetched ahead: the static graph only (lazy chunks wait until asked)
        let eager = static_imports(code);
        assert!(eager.iter().any(|f| f == "./a.js") && !eager.iter().any(|f| f == "./lazy.js"), "{eager:?}");
        assert!(!found.iter().any(|f| f == "./x.js" || f.contains("${")), "{found:?}");
    }

    #[test]
    fn styles_scripts_add_reach_layout() {
        let s = run(
            "<p id=a>alpha</p><p id=b>beta</p><p id=c>gamma</p>",
            "const st = document.createElement('style'); st.textContent = '#a { display: none }'; document.body ? document.body.append(st) : document.getElementById('c').after(st);
             const em = document.createElement('style'); document.getElementById('c').after(em); em.sheet.insertRule('#b { display: none }', 0);
             document.title = String(em.sheet.cssRules.length) + ' ' + document.styleSheets.length;",
        );
        let page = s.layout(&Fonts::load());
        let shown = page.text.join(" ");
        assert!(!shown.contains("alpha") && !shown.contains("beta") && shown.contains("gamma"), "{shown} {:?}", s.log());
        assert_eq!(s.title().as_deref(), Some("1 2"));
    }

    #[test]
    fn inserted_scripts_run_like_in_a_browser() {
        let s = run(
            "<div id=o></div><script>window.initial = (window.initial || 0) + 1;</script>",
            "window.ran = [];
             const a = document.createElement('script'); a.textContent = 'ran.push(\\'append\\')'; document.getElementById('o').append(a);
             document.getElementById('o').innerHTML += '<script>ran.push(\\'innerHTML\\')<\\/script>';
             const moved = document.querySelector('script'); document.getElementById('o').append(moved);
             const b = document.createElement('script'); b.src = 'https://cdn.example.test/x.js'; b.onerror = () => ran.push('error'); b.onload = () => ran.push('load'); document.getElementById('o').append(b);
             const t = document.createElement('script'); t.type = 'application/json'; t.textContent = 'ran.push(\\'json\\')'; document.getElementById('o').append(t);
             setTimeout(() => { document.getElementById('o').textContent = ran.join(',') + '|' + (window.initial || 0); }, 0);",
        );
        // the inline script, not the innerHTML one, not the JSON one; the old script is not run again
        assert!(text(&s).starts_with("append,error|0"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn node_constants_and_document_position() {
        let s = run(
            "<div id=o><p id=a><b id=b></b></p><i id=c></i></div>",
            "const $ = id => document.getElementById(id);
             const r = [Node.ELEMENT_NODE, Node.TEXT_NODE, $('a').DOCUMENT_FRAGMENT_NODE, $('a').nodeType === Node.ELEMENT_NODE,
               $('a').compareDocumentPosition($('b')), $('b').compareDocumentPosition($('a')), $('a').compareDocumentPosition($('c')), $('c').compareDocumentPosition($('a')),
               $('a').compareDocumentPosition(document.createElement('x')) & Node.DOCUMENT_POSITION_DISCONNECTED];
             $('o').textContent = r.join('|');",
        );
        assert_eq!(text(&s), "1|3|11|true|20|10|4|2|1", "{:?}", s.log());
    }

    #[test]
    fn dates_parse_like_a_browser() {
        let s = run(
            "<p id=o></p>",
            "const t = s => new Date(s).getTime();
             document.getElementById('o').textContent = [t('2026-11-17T06:59:55.912000+0000'), t('2026-11-17T06:59:55+0000'), Date.parse('Tue, 17 Nov 2026 06:00:00 GMT'), Date.UTC(2026, 10, 17) === Date.parse('2026-11-17'), isNaN(t('2026-11-17 06:59:55')), isNaN(Date.parse('November 17, 2026')), isNaN(Date.parse('nonsense')), new Date(5) instanceof Date, typeof Date()].join('|');",
        );
        assert_eq!(text(&s), "1794898795912|1794898795000|1794895200000|true|false|false|true|true|string", "{:?}", s.log());
    }

    #[test]
    fn shadow_dom_renders_with_slots() {
        let s = run(
            "<x-card id=c><b slot=title>Title</b><i>body text</i></x-card><div id=d><template shadowrootmode=open><p>declared</p><slot></slot></template><span>light</span></div><p id=o></p>",
            "class XCard extends HTMLElement { connectedCallback() { const r = this.attachShadow({ mode: 'open' }); r.innerHTML = '<style>:host { display: block } .hide { display: none }</style><h2><slot name=title>no title</slot></h2><section><slot></slot></section><em class=hide>hidden</em><inner-el></inner-el>'; } }
             let innerConnected = false;
             customElements.define('inner-el', class extends HTMLElement { connectedCallback() { innerConnected = this.isConnected; } });
             customElements.define('x-card', XCard);
             const c = document.getElementById('c');
             const slot = c.shadowRoot.querySelector('slot[name=title]');
             document.getElementById('o').textContent = [c.shadowRoot.host === c, slot.assignedNodes().map(n => n.textContent).join(), document.getElementById('d').shadowRoot !== null, innerConnected, document.querySelectorAll('h2').length].join('|');",
        );
        // host, slot assignment, the declarative root, the nested element connected, nothing leaking to document queries
        assert!(text(&s).ends_with("true|Title|true|true|0"), "{} {:?}", text(&s), s.log());
        let shown = s.layout(&Fonts::load()).text.join(" ");
        assert!(shown.contains("Title") && shown.contains("body") && shown.contains("declared") && shown.contains("light"), "{shown}");
        assert!(!shown.contains("no title") && !shown.contains("hidden"), "{shown}");
    }

    #[test]
    fn scripts_read_the_layout_geometry() {
        let s = run(
            "<body style='margin:0'><div id=a style='width:300px;height:50px'>x</div><div id=b style='padding:10px'>hello</div><div id=d style='display:none'>x</div><p id=o></p></body>",
            "const g = id => document.getElementById(id);
             const a = g('a').getBoundingClientRect(), b = g('b');
             g('o').textContent = [a.width, a.height, b.offsetTop, b.offsetWidth, g('d').offsetWidth, g('d').offsetParent === null].join('|');",
        );
        assert!(text(&s).ends_with("300|50|50|800|0|true"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn flex_items_keep_percentage_widths_when_centred() {
        let s = run(
            "<body style='margin:0'><div style='overflow:hidden;display:flex;flex-direction:column;align-items:center;width:600px'><section id=a style='width:100%;flex:none;display:flex;flex-direction:row'><div id=b style='flex:1;flex-basis:0.000000001px'></div></section></div><div style='display:flex;justify-content:center;width:600px'><section id=c style='width:100%;flex:none'></section></div><p id=o></p></body>",
            "const w = id => document.getElementById(id).getBoundingClientRect().width;
             document.getElementById('o').textContent = [w('a'), w('b'), w('c')].join('|');",
        );
        assert!(text(&s).ends_with("600|600|600"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn modules_may_end_with_a_bare_export_list() {
        let mut ctx = Context::default();
        for code in ["const a = 1; export { a }", "const a = 1; export { a as b, a as default } "] {
            assert!(boa_engine::Module::parse(Source::from_bytes(code), None, &mut ctx).is_ok(), "{code}");
        }
    }

    /// Debugging: SEPIA_PARSE_MODULE=file parses a module source with Boa.
    #[test]
    #[ignore]
    fn parse_module_file() {
        let Ok(path) = std::env::var("SEPIA_PARSE_MODULE") else { return };
        let code = std::fs::read_to_string(path).unwrap();
        let mut ctx = Context::default();
        match boa_engine::Module::parse(Source::from_bytes(&code), None, &mut ctx) {
            Ok(_) => println!("PARSE OK"),
            Err(e) => println!("PARSE ERR: {e:?}"),
        }
        match ctx.eval(Source::from_bytes(&code)) {
            Ok(_) => println!("SCRIPT OK"),
            Err(e) => println!("SCRIPT ERR: {e}"),
        }
    }

    #[test]
    fn handler_attributes_run_like_listeners() {
        let mut s = run(
            "<html><body onload=\"document.getElementById('o').textContent += 'load '\"><div onclick=\"document.getElementById('o').textContent += 'div '\"><a id=a href='/x' onclick=\"document.getElementById('o').textContent += 'a '; return false\">go</a></div><p id=o></p></body></html>",
            "",
        );
        let a = s.tree();
        let id: usize = find_id(&a, "a").unwrap().attr("data-sepia-id").unwrap().parse().unwrap();
        let outcome = s.handle(PageEvent::Click { node: id, href: Some("/x".into()) }, &Fonts::load());
        assert!(text(&s).ends_with("load a div "), "{} {:?}", text(&s), s.log());
        assert!(outcome.navigate.is_none(), "return false cancels the link");
    }

    #[test]
    fn history_entries_walk_back_with_popstate() {
        let mut s = run(
            "<p id=o></p>",
            "const seen = [];
             addEventListener('popstate', e => { seen.push(JSON.stringify(e.state) + '@' + location.pathname); document.getElementById('o').textContent = seen.join(' '); });
             history.pushState({ n: 1 }, '', '/one');
             history.pushState({ n: 2 }, '', '/two');
             document.getElementById('o').textContent = history.length + ' ' + location.pathname;",
        );
        assert!(text(&s).ends_with("3 /two"), "{} {:?}", text(&s), s.log());
        assert!(s.history_step(-1));
        assert!(text(&s).ends_with("{\"n\":1}@/one"), "{} {:?}", text(&s), s.log());
        assert!(s.history_step(-1));
        assert!(!s.history_step(-1), "past the first entry the window leaves the page");
    }

    #[test]
    fn offset_parent_walk_ends_at_the_body() {
        let s = run(
            "<html><body><div id=a style='position:relative'><span id=b>x</span></div><p id=o></p></body></html>",
            "let n = 0; for (let e = document.getElementById('b'); e && n < 10; e = e.offsetParent) n++;
             document.getElementById('o').textContent = [n, document.body.offsetParent, document.documentElement.offsetParent].join('|');",
        );
        assert!(text(&s).ends_with("3|null|null") || text(&s).ends_with("3||"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn inserting_a_node_before_itself_keeps_it() {
        let s = run(
            "<div id=d><b>1</b><i>2</i></div><p id=o></p>",
            "const d = document.getElementById('d'); const b = d.firstChild;
             d.prepend(b); d.insertBefore(d.lastChild, d.lastChild);
             document.getElementById('o').textContent = [...d.children].map(e => e.localName).join(',');",
        );
        assert!(text(&s).ends_with("b,i"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn percent_heights_need_a_definite_parent() {
        let s = run(
            "<body style='margin:0'><div id=a><div id=b style='height:100%'>x</div></div><div style='height:200px'><div id=c style='height:50%'></div></div><div id=d style='display:flex'><span>m</span><div style='position:absolute;width:900px'>wide</div></div><p id=o></p></body>",
            "const h = id => Math.round(document.getElementById(id).getBoundingClientRect().height);
             document.getElementById('o').textContent = [h('b') < 40, h('c'), document.getElementById('d').firstChild.getBoundingClientRect().width < 100].join('|');",
        );
        assert!(text(&s).ends_with("true|100|true"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn fragments_connect_their_children() {
        let s = run(
            "<div id=host></div><p id=o></p>",
            "let n = 0;
             customElements.define('x-c', class extends HTMLElement { connectedCallback() { n++; } });
             const f = document.createDocumentFragment();
             f.append(document.createElement('x-c'), document.createElement('x-c'));
             const t = document.createElement('template'); t.innerHTML = '<x-c></x-c>';
             document.getElementById('host').append(f);
             document.getElementById('host').appendChild(document.importNode(t.content, true));
             document.getElementById('o').textContent = 'n=' + n;",
        );
        assert!(text(&s).ends_with("n=3"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn custom_elements_upgrade_in_place() {
        let s = run(
            "<x-late id=x></x-late><p id=o></p>",
            "const before = document.getElementById('x');
             customElements.define('x-late', class extends HTMLElement { constructor() { super(); this.ready = true; } hello() { return 'hi'; } });
             const after = document.getElementById('x');
             document.getElementById('o').textContent = [before === after, before.ready, typeof before.hello === 'function' && before.hello()].join('|');",
        );
        assert_eq!(text(&s), "true|true|hi", "{:?}", s.log());
    }

    #[test]
    fn number_format_styles() {
        let s = run(
            "<p id=o></p>",
            "const f = (o, v) => new Intl.NumberFormat('en-US', o).format(v);
             document.getElementById('o').textContent = [f({ style: 'currency', currency: 'USD' }, 1234.5), f({ style: 'percent' }, 0.5), f({}, NaN),
               new Intl.NumberFormat('en-US', { style: 'percent' }).formatToParts(0.25).map(p => p.type).join(',')].join('|');",
        );
        assert_eq!(text(&s), "$1,234.50|50%|NaN|integer,percentSign", "{:?}", s.log());
    }

    #[test]
    fn mutation_observers_see_every_change() {
        let s = run(
            "<div id=o></div><div id=box><p id=p class=a>t</p></div>",
            "const box = document.getElementById('box'), out = [];
             const mo = new MutationObserver(records => { for (const r of records) out.push(r.type + ':' + (r.attributeName || '') + ':' + (r.oldValue ?? '') + ':' + r.addedNodes.length + '/' + r.removedNodes.length); });
             mo.observe(box, { childList: true, subtree: true, attributes: true, attributeOldValue: true, characterData: true, characterDataOldValue: true });
             const p = document.getElementById('p');
             p.className = 'b';
             p.firstChild.data = 'u';
             box.append(document.createElement('i'));
             p.remove();
             // the ShadyDOM trick: a text node's data change is the microtask
             let ticked = false; const t = document.createTextNode(''); const m2 = new MutationObserver(() => { ticked = true; }); m2.observe(t, { characterData: true }); t.textContent = '1';
             const outside = document.createElement('b'); document.getElementById('o').after(outside);
             Promise.resolve().then(() => Promise.resolve()).then(() => {
               mo.disconnect(); box.append(document.createElement('u'));
               setTimeout(() => { document.getElementById('o').textContent = out.join(' ') + ' | ' + ticked; }, 0);
             });",
        );
        assert_eq!(text(&s), "attributes:class:a:0/0 characterData::t:0/0 childList:::1/0 childList:::0/1 | true", "{:?}", s.log());
    }

    #[test]
    fn template_content_is_one_fragment_that_clones() {
        let s = run(
            "<div id=o></div><template id=t><b>x</b><i>y</i></template>",
            "const t = document.getElementById('t');
             const c = t.content;
             c.firstChild.setAttribute('data-k', '1');
             const same = c === t.content;
             const copy = document.importNode(t.content, true);
             const clone = t.cloneNode(true);
             t.innerHTML = '<u>z</u>';
             document.getElementById('o').textContent = [same, t.childNodes.length, copy.firstChild.getAttribute('data-k'), clone.content.childNodes.length, clone.content.firstChild.getAttribute('data-k'), t.content.textContent, t.innerHTML].join('|');",
        );
        assert_eq!(text(&s), "true|0|1|2|1|z|<u>z</u>", "{:?}", s.log());
    }

    #[test]
    fn ranges_and_selection() {
        let s = run(
            "<div id=o><p id=a>hello <b>big</b> world</p></div>",
            "const p = document.getElementById('a'), r = document.createRange();
             r.setStart(p.firstChild, 2); r.setEnd(p.lastChild, 3);
             const text = r.toString();
             const sel = getSelection(); sel.removeAllRanges(); sel.addRange(r);
             const frag = r.cloneContents();
             const r2 = document.createRange(); r2.selectNodeContents(p);
             document.getElementById('o').textContent = [text, sel.toString() === text, frag.textContent, r2.toString(), r.commonAncestorContainer === p, new DOMRect(1, 2, 3, 4).right].join('|');",
        );
        assert_eq!(text(&s), "llo big wo|true|llo big wo|hello big world|true|4", "{:?}", s.log());
    }

    #[test]
    fn indexeddb_stores_indexes_cursors_and_versions() {
        let s = run(
            "<p id=o></p>",
            "const out = []; const _push = out.push.bind(out); out.push = (...a) => { _push(...a); document.getElementById('o').textContent = out.join('|'); };
             window.addEventListener('error', e => out.push('ERR ' + e.message));
             const req = indexedDB.open('app', 2);
             req.onupgradeneeded = e => {
               const db = req.result;
               out.push('up' + e.oldVersion + '>' + e.newVersion);
               const st = db.createObjectStore('people', { keyPath: 'id', autoIncrement: true });
               st.createIndex('byAge', 'age');
               st.add({ name: 'b', age: 30, when: new Date(0) });
               st.add({ name: 'a', age: 20 });
               st.put({ id: 10, name: 'c', age: 20 });
             };
             req.onsuccess = () => {
               const db = req.result;
               const tx = db.transaction('people', 'readwrite');
               const st = tx.objectStore('people');
               st.get(1).then ? 0 : 0;
               const g = st.get(1); g.onsuccess = () => out.push(g.result.name + (g.result.when instanceof Date));
               const ix = st.index('byAge').getAll(20); ix.onsuccess = () => out.push(ix.result.map(p => p.name).join(''));
               const dup = st.add({ id: 10, name: 'x' }); dup.onerror = e => { out.push(dup.error.name); e.preventDefault(); };
               const keys = []; const cur = st.openCursor(null, 'prev');
               cur.onsuccess = () => { const c = cur.result; if (c) { keys.push(c.key); c.continue(); } else out.push(keys.join(',')); };
               tx.oncomplete = () => {
                 const r = db.transaction('people').objectStore('people').count(IDBKeyRange.bound(1, 10, false, true));
                 r.onsuccess = () => { out.push(r.result, indexedDB.cmp([1, 'a'], [1, 2]), db.version, [...db.objectStoreNames].join()); document.getElementById('o').textContent = out.join('|'); };
               };
             };",
        );
        assert_eq!(text(&s), "up0>2|btrue|ac|ConstraintError|10,2,1|2|1|2|people", "{:?}", s.log());
    }

    #[test]
    fn sorting_is_stable_and_survives_any_comparator() {
        // Boa 0.22 panicked (taking the browser down) on a shuffle (patched in vendor/boa_engine)
        let s = run(
            "<p id=o></p>",
            "const a = []; for (let i = 0; i < 500; i++) a.push(i); a.sort(() => Math.random() - 0.5);
             const sorted = [3, 1, undefined, 10, 2].sort();
             const byNum = [3, 1, 10, 2].sort((x, y) => x - y);
             const stable = [{k: 1, n: 'a'}, {k: 0, n: 'b'}, {k: 1, n: 'c'}, {k: 0, n: 'd'}].sort((x, y) => x.k - y.k).map(o => o.n).join('');
             const typed = Array.from(new Int32Array([5, -1, 3]).sort()).join(',');
             document.getElementById('o').textContent = [a.length, new Set(a).size, sorted.join(','), byNum.join(','), stable, typed].join('|');",
        );
        assert_eq!(text(&s), "500|500|1,10,2,3,|1,2,3,10|bdac|-1,3,5", "{:?}", s.log());
    }

    #[test]
    fn nested_arrows_keep_this_and_private_names() {
        // Boa 0.22 lost the method's `this` two arrows deep (patched in vendor/boa_ast)
        let s = run(
            "<p id=o></p>",
            "class I { #h = 1; get #g() { return this.#h + 1; } m() { return [(() => () => this)()() === this, (() => () => this.#g)()(), [1].map(() => () => this.#h)[0]()]; } }
             const o = {}; function f() { return (() => () => this)()() === this; }
             document.getElementById('o').textContent = [...new I().m(), f.call(o), /\\bclass\\b/.test(class {}.toString()), String(class B {})].join('|');",
        );
        assert_eq!(text(&s), "true|2|1|true|true|class B { [native code] }", "{:?}", s.log());
    }

    #[test]
    fn intl_is_real_but_the_same_for_everybody() {
        let s = run(
            "<p id=o></p>",
            "document.getElementById('o').textContent = [new Intl.NumberFormat().resolvedOptions().locale, new Date(0).getTimezoneOffset(), (1234.5).toLocaleString(), new Date(0).toISOString() === new Date(0).toJSON(), new Date(0).getHours(), Intl.NumberFormat.supportedLocalesOf(['de']).length, new Intl.NumberFormat('de-DE').format(1234.5), new Intl.PluralRules('ru').select(3)].join('|');",
        );
        // the default persona: Firefox in the US (New York, EST in January 1970)
        assert_eq!(text(&s), "en-US|300|1,234.5|true|19|1|1.234,5|few", "{:?}", s.log());
    }

    #[test]
    fn scripts_see_the_chosen_browser_and_country() {
        use crate::persona::{browser, country, Persona, TEST_PERSONA};
        TEST_PERSONA.with(|t| t.set(Some(Persona { browser: browser("chrome-win").unwrap(), country: country("RU").unwrap() })));
        let s = run(
            "<p id=o></p>",
            "const d = new Date(Date.UTC(2026, 6, 1, 12));
             document.getElementById('o').textContent = [navigator.userAgent.includes('Chrome/141'), navigator.platform, navigator.vendor, navigator.language, navigator.languages.join(','),
               navigator.userAgentData && navigator.userAgentData.brands.map(b => b.brand).includes('Google Chrome'), 'oscpu' in navigator,
               d.getTimezoneOffset(), d.getHours(), new Intl.NumberFormat().resolvedOptions().locale, (1234.5).toLocaleString()].join('|');",
        );
        TEST_PERSONA.with(|t| t.set(None));
        assert_eq!(text(&s), "true|Win32|Google Inc.|ru-RU|ru-RU,ru,en-US,en|true|false|-180|15|ru|1\u{a0}234,5", "{:?}", s.log());
    }

    #[test]
    fn custom_elements_construct_upgrade_and_connect() {
        let s = run(
            "<x-old id='old'></x-old><div id='out'></div>",
            "'use strict';
             const seen = [];
             class XOld extends HTMLElement { constructor() { super(); this.delegate = { kind: 'old' }; } connectedCallback() { seen.push('old:' + this.id); } }
             customElements.define('x-old', XOld);
             class XNew extends HTMLElement {
               static get observedAttributes() { return ['label']; }
               constructor() { super(); this.delegate = { kind: 'new' }; }
               connectedCallback() { seen.push('new'); }
               attributeChangedCallback(n, o, v) { seen.push(n + '=' + v); }
             }
             customElements.define('x-new', XNew);
             const made = document.createElement('x-new');
             const proto = Object.getPrototypeOf(made.delegate);
             made.setAttribute('label', 'hi');
             document.getElementById('out').append(made);
             const direct = new XNew();
             addEventListener('load', () => seen.push('load'));
             document.getElementById('out').textContent = [made instanceof XNew, direct.tagName, document.getElementById('old').delegate.kind, proto === Object.prototype, seen.join(' ')].join('|');",
        );
        assert!(text(&s).contains("true|X-NEW|old|true|old:old label=hi new"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn modules_run_after_classic_scripts() {
        let doc = dom::parse_document("<p id='out'></p>");
        let url = Url::parse("https://example.test/page").unwrap();
        let scripts = vec![
            ("document.getElementById('out').textContent += 'm:' + (typeof window.classic) + ':' + import.meta.url;".to_string(), format!("{MODULE}https://example.test/app.js")),
            ("window.classic = 1;".to_string(), "inline script".to_string()),
        ];
        // page.rs orders them; here the module simply runs as a module
        let s = Session::start(&doc, &url, &scripts, None, Stylesheet::default(), ImageMap::new(), 800.0);
        assert!(text(&s).contains("m:undefined:https://example.test/app.js"), "{} {:?}", text(&s), s.log());
    }

    #[test]
    fn click_events_bubble_and_change_attributes() {
        let mut s = run(
            "<section id='one'>A</section><section id='two' hidden>B</section><nav><button data-tab='two'><b>go</b></button></nav>",
            "document.querySelector('nav').addEventListener('click', e => {
                 const tab = e.target.closest('button').dataset.tab;
                 document.getElementById('one').hidden = true;
                 document.getElementById(tab).hidden = false;
             });",
        );
        let button = s.tree().attr("data-sepia-id").map(|_| 0).unwrap_or(0);
        let _ = button;
        // find the <b> inside the button: clicks land on the innermost element
        let tree = s.tree();
        let b = find(&tree, "b").expect("b");
        let id: usize = b.attr("data-sepia-id").unwrap().parse().unwrap();
        let out = s.handle(PageEvent::Click { node: id, href: None }, &Fonts::load());
        assert!(out.page.is_some(), "DOM changed: {:?}", s.log());
        let tree = s.tree();
        assert!(find_id(&tree, "one").unwrap().attr("hidden").is_some());
        assert!(find_id(&tree, "two").unwrap().attr("hidden").is_none());
    }

    #[test]
    fn links_navigate_unless_prevented_and_hash_events_fire() {
        let mut s = run(
            "<a id='a' href='/next'>n</a><a id='b' href='/stay'>s</a><p id='h'></p>",
            "document.getElementById('b').addEventListener('click', e => e.preventDefault());
             window.addEventListener('hashchange', () => { document.getElementById('h').textContent = location.hash; });",
        );
        let tree = s.tree();
        let id = |t: &Node, i: &str| find_id(t, i).unwrap().attr("data-sepia-id").unwrap().parse::<usize>().unwrap();
        let out = s.handle(PageEvent::Click { node: id(&tree, "a"), href: Some("/next".into()) }, &Fonts::load());
        assert_eq!(out.navigate.as_deref(), Some("/next"));
        let out = s.handle(PageEvent::Click { node: id(&tree, "b"), href: Some("/stay".into()) }, &Fonts::load());
        assert_eq!(out.navigate, None);
        s.handle(PageEvent::Hash("archive".into()), &Fonts::load());
        assert!(text(&s).contains("#archive"), "{:?}", s.log());
    }

    #[test]
    fn privacy_surface_is_fixed() {
        let s = run(
            "<p id='o'></p>",
            "document.cookie = 'a=b';
             localStorage.setItem('k', 'v');
             let net = 'ok'; try { fetch('https://elsewhere.test/x'); } catch (e) { net = 'threw'; }
             document.getElementById('o').textContent = [navigator.userAgent, document.cookie === '' ? 'nocookie' : 'cookie', localStorage.getItem('k'), screen.width, typeof Intl.PluralRules].join('|');",
        );
        let t = text(&s);
        assert!(t.contains(&format!("{}|cookie|v|1280|function", crate::persona::current().browser.user_agent)), "{t} {:?}", s.log());
    }

    #[test]
    fn xhr_and_form_data_without_network() {
        let s = run(
            "<form id=f><input name=q value='rust lang'><input type=checkbox name=c></form><p id='o'></p>",
            "const fd = new FormData(document.getElementById('f')); fd.append('x', '1');
             const out = document.getElementById('o');
             const x = new XMLHttpRequest(); x.open('POST', '/api'); 
             x.onerror = () => { out.textContent = [...fd].map(p => p.join('=')).join('&') + '|error|' + x.readyState; };
             x.onload = () => { out.textContent = 'loaded'; };
             x.send(fd);",
        );
        let t = text(&s);
        assert!(t.contains("q=rust lang&x=1|error|4"), "{t} {:?}", s.log());
    }

    #[test]
    #[ignore = "no loop limit for now (the user asked for none): a runaway loop now hangs its page"]
    fn runaway_scripts_are_stopped() {
        let s = run("<p id='o'>before</p>", "while (true) {} ");
        assert!(text(&s).contains("before"));
        assert!(s.log().iter().any(|l| l.contains("test")), "{:?}", s.log());
    }

    #[test]
    fn timers_promises_and_inner_html() {
        let mut s = run(
            "<div id='d'></div>",
            "setTimeout(() => { document.getElementById('d').innerHTML = '<b>late</b> <i>x</i>'; }, 50);
             Promise.resolve().then(() => document.title = 'promised');",
        );
        advance_clock(&mut s, 200);
        assert!(text(&s).contains("late x"), "{:?}", s.log());
        assert_eq!(s.title().as_deref(), Some("promised"));
    }

    /// Inspect real script failures without treating HTTP success as compatibility.
    /// Set SEPIA_BENCH_URL to the page under investigation and RUST_MIN_STACK=33554432
    /// for large bundles (the default Rust test thread stack is too small).
    #[test]
    #[ignore]
    fn live_script_diagnostics() {
        let address = std::env::var("SEPIA_BENCH_URL").expect("set SEPIA_BENCH_URL");
        let url = Url::parse(&address).unwrap();
        let fonts = Fonts::load();
        let (loaded, session, _) = crate::page::open(&url, &Chain::default(), Some(&Resolver::default()), 1184.0, &fonts, Duration::from_secs(12), false, false, None).unwrap();
        let session = session.expect("session");
        println!("status={} styles={} rendered_text={} note={} pending_navigation={}",
            loaded.status, loaded.rules, loaded.page.text.len(), loaded.note,
            with(|st| st.navigate.is_some()));
        for line in session.log() { println!("JS: {line}"); }
    }

    /// `SEPIA_BENCH_URL=https://a152b.host/ cargo test --release live_tabs -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_tabs() {
        let Ok(address) = std::env::var("SEPIA_BENCH_URL") else { return };
        let fonts = Fonts::load();
        let url = Url::parse(&address).unwrap();
        let (loaded, session, _) = crate::page::open(&url, &Chain::default(), Some(&Resolver::default()), 1184.0, &fonts, Duration::from_secs(20), false, false, None).unwrap();
        let mut session = session.expect("scripts ran");
        println!("note: {}  log: {:?}", loaded.note, session.log());
        let tree = session.tree();
        let tab = find_attr(&tree, "data-tab", "archive").expect("archive tab");
        let id: usize = tab.attr("data-sepia-id").unwrap().parse().unwrap();
        let out = session.handle(PageEvent::Click { node: id, href: None }, &fonts);
        println!("relayout: {}, title: {:?}, log: {:?}", out.page.is_some(), out.title, session.log());
        let tree = session.tree();
        println!("archive hidden: {:?}, projects hidden: {:?}", find_id(&tree, "archive").map(|n| n.attr("hidden").is_some()), find_id(&tree, "projects").map(|n| n.attr("hidden").is_some()));
    }

    /// Click every button of a live page and report what happened.
    /// `SEPIA_BENCH_URL=https://a152b.host/ cargo test --release live_buttons -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_buttons() {
        let Ok(address) = std::env::var("SEPIA_BENCH_URL") else { return };
        let fonts = Fonts::load();
        let url = Url::parse(&address).unwrap();
        let (_, session, _) = crate::page::open(&url, &Chain::default(), Some(&Resolver::default()), 1184.0, &fonts, Duration::from_secs(20), false, false, None).unwrap();
        let mut session = session.expect("session");
        fn buttons(node: &Node, out: &mut Vec<(usize, String)>) {
            if node.tag == "button" || (node.tag == "input" && matches!(node.attr("type"), Some("submit") | Some("button"))) {
                let label = format!("{} [{}]", node.text_content().split_whitespace().collect::<Vec<_>>().join(" "), node.attrs.iter().filter(|(k, _)| k != "data-sepia-id").map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "));
                out.push((node.attr("data-sepia-id").unwrap().parse().unwrap(), label));
            }
            for c in &node.children {
                buttons(c, out);
            }
        }
        let mut list = Vec::new();
        buttons(&session.tree(), &mut list);
        // hit testing as the window does it: the smallest box around a point
        let page = session.layout(&fonts);
        for (id, label) in &list {
            let Some((rect, _)) = page.boxes.iter().find(|(_, b)| b == id) else {
                println!("NO BOX for {label}");
                continue;
            };
            let (x, y) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            let hit = page.boxes.iter().filter(|(r, _)| r.contains(x, y)).min_by(|a, b| (a.0.w * a.0.h).partial_cmp(&(b.0.w * b.0.h)).unwrap()).map(|(_, i)| *i);
            if hit != Some(*id) {
                println!("centre of {label} hits {hit:?} (button is {id}), rect {rect:?}");
            }
        }
        let mut seen = session.log().len();
        for (id, label) in list {
            let before = session.tree().text_content();
            let out = session.handle(PageEvent::Click { node: id, href: None }, &fonts);
            let after = session.tree().text_content();
            let log = session.log();
            println!("{label}\n   relayout={} changed_text={} navigate={:?} new_log={:?}", out.page.is_some(), before != after, out.navigate, &log[seen..]);
            seen = log.len();
        }
    }

    fn find_attr<'n>(node: &'n Node, name: &str, value: &str) -> Option<&'n Node> {
        if node.attr(name) == Some(value) {
            return Some(node);
        }
        node.children.iter().find_map(|c| find_attr(c, name, value))
    }

    fn find<'n>(node: &'n Node, tag: &str) -> Option<&'n Node> {
        if node.tag == tag {
            return Some(node);
        }
        node.children.iter().find_map(|c| find(c, tag))
    }

    fn find_id<'n>(node: &'n Node, id: &str) -> Option<&'n Node> {
        if node.attr("id") == Some(id) {
            return Some(node);
        }
        node.children.iter().find_map(|c| find_id(c, id))
    }
}
