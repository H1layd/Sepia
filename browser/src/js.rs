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
//!   user, no canvas, WebGL, audio, battery, devices or high-resolution time
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
}

/// A mutable document: node 0 is `#document`.
#[derive(Debug, Clone, Default)]
pub struct Dom {
    nodes: Vec<DNode>,
}

impl Dom {
    pub fn from_tree(root: &Node) -> Dom {
        let mut dom = Dom { nodes: Vec::new() };
        dom.adopt(root, None);
        dom
    }

    fn adopt(&mut self, node: &Node, parent: Option<usize>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(DNode { tag: node.tag.clone(), attrs: node.attrs.clone(), text: node.text.clone(), parent, children: Vec::new() });
        for child in &node.children {
            let c = self.adopt(child, Some(id));
            self.nodes[id].children.push(c);
        }
        id
    }

    /// The tree for layout, every element tagged with `data-sepia-id`.
    pub fn to_tree(&self) -> Node {
        self.build(0)
    }

    fn build(&self, id: usize) -> Node {
        let n = &self.nodes[id];
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
        self.nodes.push(DNode { tag: tag.to_string(), attrs: Vec::new(), text, parent: None, children: Vec::new() });
        self.nodes.len() - 1
    }

    fn detach(&mut self, id: usize) {
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
        self.detach(child);
        let at = before.and_then(|b| self.nodes[parent].children.iter().position(|c| *c == b)).unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(at, child);
        self.nodes[child].parent = Some(parent);
    }

    fn text_content(&self, id: usize) -> String {
        let n = &self.nodes[id];
        match &n.text {
            Some(t) => t.clone(),
            None => n.children.iter().map(|c| self.text_content(*c)).collect(),
        }
    }

    fn set_text(&mut self, id: usize, text: String) {
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
        if let Some(t) = &n.text {
            return escape(t, false);
        }
        let inner: String = n.children.iter().map(|c| self.html(*c, true)).collect();
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
    fn query(&self, scope: usize, selectors: &str, all: bool) -> Vec<usize> {
        let list: Vec<Selector> = split_top_level(selectors, ',').iter().filter_map(|s| Selector::parse(s)).collect();
        if list.is_empty() {
            return Vec::new();
        }
        let tree = self.to_tree();
        let mut out = Vec::new();
        fn walk(node: &Node, ancestors: &mut Vec<*const Node>, list: &[Selector], scope: usize, inside: bool, all: bool, out: &mut Vec<usize>) {
            let id = node.attr("data-sepia-id").and_then(|v| v.parse::<usize>().ok());
            let inside_now = inside || (scope == 0 && node.tag == "#document");
            if inside && node.text.is_none() && !node.tag.starts_with('#') {
                // SAFETY: the pointers are to nodes of `tree`, alive for the whole walk
                let anc: Vec<&Node> = ancestors.iter().map(|p| unsafe { &**p }).collect();
                if list.iter().any(|s| matches_selector(s, node, &anc)) {
                    if let Some(id) = id {
                        out.push(id);
                        if !all {
                            return;
                        }
                    }
                }
            }
            ancestors.push(node as *const Node);
            let child_inside = inside_now || id == Some(scope);
            for child in &node.children {
                if !all && !out.is_empty() {
                    break;
                }
                walk(child, ancestors, list, scope, child_inside, all, out);
            }
            ancestors.pop();
        }
        walk(&tree, &mut Vec::new(), &list, scope, false, all, &mut out);
        out
    }

    fn matches(&self, id: usize, selectors: &str) -> bool {
        let Some(parent) = self.nodes[id].parent else { return false };
        // matching needs ancestors: query from the root and look for `id`
        let _ = parent;
        self.query(0, selectors, true).contains(&id)
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
    log: Vec<String>,
    dirty: bool,
    net: Option<Net>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
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

fn register(ctx: &mut Context) {
    native!(ctx, "__dom_kind", 1, |_, a, _| {
        let id = arg_id(a, 0);
        Ok(with(|st| match st.dom.valid(id) {
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
                for c in std::mem::take(&mut st.dom.nodes[i].children) {
                    st.dom.nodes[c].parent = None;
                }
                st.dirty = true;
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
    native!(ctx, "__log", 1, |_, a, ctx| {
        let line = arg_str(a, 0, ctx)?;
        with(|st| {
            if st.log.len() < 200 {
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
    native!(ctx, "__fetch", 4, |_, a, ctx| {
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
        let result = with(|st| -> Result<(u16, String, String, String), String> {
            let net = st.net.as_ref().ok_or("network is off")?;
            let base = st.url.clone().unwrap_or_else(|| net.origin.clone());
            let url = base.resolve(&target).ok_or("bad URL")?;
            let site = crate::cookies::site_of(&net.origin.host);
            if url.scheme != net.origin.scheme || crate::cookies::site_of(&url.host) != site {
                return Err(format!("blocked: {} is not this page's site", url.host));
            }
            let request = PageRequest { site: &site, form: body.as_deref(), content_type: content_type.as_deref(), method: method.as_deref() };
            let response = fetch_in(&url, &net.chain, net.resolver.as_ref(), net.timeout, &request)?;
            let kind = response.header("content-type").unwrap_or("").to_string();
            Ok((response.status, String::from_utf8_lossy(&response.body).into_owned(), response.final_url.clone(), kind))
        });
        match result {
            Ok((status, body, url, kind)) => Ok(JsArray::from_iter([JsValue::from(status as f64), s(body), s(url), s(kind)], ctx).into()),
            Err(e) => Err(boa_engine::JsNativeError::typ().with_message(format!("fetch failed: {e}")).into()),
        }
    });
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Something the user did to the page.
#[derive(Debug, Clone)]
pub enum PageEvent {
    /// A click on element `node` (its `data-sepia-id`); `href` if it was inside a link.
    Click { node: usize, href: Option<String> },
    /// Text typed into (or deleted from) a focused field.
    Input { node: usize, value: String },
    Key { node: usize, key: String },
    /// The fragment changed (a same-page `#link`).
    #[allow(dead_code)]
    Hash(String),
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
}

/// One live page: DOM, scripts, styles, images.
pub struct Session {
    ctx: Context,
    sheet: Stylesheet,
    images: ImageMap,
    width: f32,
}

/// What a script may use of the network: the page's origin and path.
pub struct NetAccess<'a> {
    pub chain: &'a Chain,
    pub resolver: Option<&'a Resolver>,
    pub timeout: Duration,
}

/// Scripts of a document in order: (source text, label for errors).
pub type Scripts = Vec<(String, String)>;

impl Session {
    /// Build the DOM, run the page's scripts, fire DOMContentLoaded and load,
    /// drain timers. Returns the session and the log.
    #[allow(clippy::too_many_arguments)]
    pub fn start(document: &Node, url: &Url, scripts: &Scripts, net: Option<NetAccess>, sheet: Stylesheet, images: ImageMap, width: f32) -> Session {
        with(|st| {
            *st = State::default();
            st.dom = Dom::from_tree(document);
            st.url = Some(url.clone());
            st.net = net.map(|n| Net { origin: url.clone(), chain: n.chain.clone(), resolver: n.resolver.cloned(), timeout: n.timeout.min(Duration::from_secs(10)) });
        });
        let mut ctx = Context::default();
        {
            let limits = ctx.runtime_limits_mut();
            limits.set_loop_iteration_limit(20_000_000);
            limits.set_recursion_limit(1024);
        }
        register(&mut ctx);
        let viewport = format!("globalThis.__viewport = [{width}, 800];");
        let _ = ctx.eval(Source::from_bytes(&viewport));
        if let Err(e) = ctx.eval(Source::from_bytes(PRELUDE)) {
            with(|st| st.log.push(format!("prelude: {e}")));
        }
        for (code, label) in scripts {
            if let Err(e) = ctx.eval(Source::from_bytes(code)) {
                with(|st| st.log.push(format!("{label}: {e}")));
            }
            let _ = ctx.run_jobs();
        }
        let _ = ctx.eval(Source::from_bytes("__sepia_loaded()"));
        let mut session = Session { ctx, sheet, images, width };
        session.settle();
        session
    }

    /// Run promise jobs and due timers until things are quiet (bounded).
    fn settle(&mut self) {
        for _ in 0..8 {
            let _ = self.ctx.run_jobs();
            match self.ctx.eval(Source::from_bytes("__sepia_tick()")) {
                Ok(v) if v.as_number().unwrap_or(0.0) > 0.0 => continue,
                _ => break,
            }
        }
        let _ = self.ctx.run_jobs();
    }

    /// The document as it is now, for layout.
    pub fn tree(&self) -> Node {
        with(|st| st.dom.to_tree())
    }

    pub fn layout(&self, fonts: &Fonts) -> Page {
        let tree = self.tree();
        Layout::new(fonts, &self.sheet, self.width).with_images(&self.images).build(&tree)
    }

    /// Images that arrived after the page was first shown.
    pub fn set_images(&mut self, images: ImageMap) {
        self.images = images;
    }

    pub fn title(&self) -> Option<String> {
        with(|st| st.title.clone())
    }

    pub fn log(&self) -> Vec<String> {
        with(|st| st.log.clone())
    }

    /// Hand an event to the page's scripts and report what changed.
    pub fn handle(&mut self, event: PageEvent, fonts: &Fonts) -> Outcome {
        with(|st| {
            st.dirty = false;
            st.navigate = None;
            st.form = None;
            st.focus = None;
        });
        let call = match &event {
            PageEvent::Click { node, href } => format!(
                "__sepia_click({node}, {})",
                href.as_ref().map(|h| json_string(h)).unwrap_or_else(|| "null".into())
            ),
            PageEvent::Input { node, value } => format!("__sepia_input({node}, {})", json_string(value)),
            PageEvent::Key { node, key } => format!("__sepia_key({node}, {})", json_string(key)),
            PageEvent::Hash(hash) => format!("__sepia_hash({})", json_string(hash)),
        };
        if let Err(e) = self.ctx.eval(Source::from_bytes(&call)) {
            with(|st| st.log.push(format!("event: {e}")));
        }
        self.settle();
        let (dirty, navigate, form, focus, title) = with(|st| {
            let focus = st.focus.and_then(|f| {
                let tag = st.dom.nodes[f].tag.clone();
                matches!(tag.as_str(), "input" | "textarea").then(|| (f, st.dom.attr(f, "value").unwrap_or("").to_string()))
            });
            (st.dirty, st.navigate.take(), st.form.take(), focus, st.title.clone())
        });
        Outcome { page: dirty.then(|| self.layout(fonts)), title, navigate, form, focus }
    }
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

    fn run(html: &str, script: &str) -> Session {
        let doc = dom::parse_document(html);
        let url = Url::parse("https://example.test/page").unwrap();
        Session::start(&doc, &url, &vec![(script.to_string(), "test".into())], None, Stylesheet::default(), ImageMap::new(), 800.0)
    }

    fn text(session: &Session) -> String {
        session.tree().text_content()
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
        assert!(t.contains("sepia-browser/0.1|cookie|v|1280|function"), "{t} {:?}", s.log());
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
    fn runaway_scripts_are_stopped() {
        let s = run("<p id='o'>before</p>", "while (true) {} ");
        assert!(text(&s).contains("before"));
        assert!(s.log().iter().any(|l| l.contains("test")), "{:?}", s.log());
    }

    #[test]
    fn timers_promises_and_inner_html() {
        let s = run(
            "<div id='d'></div>",
            "setTimeout(() => { document.getElementById('d').innerHTML = '<b>late</b> <i>x</i>'; }, 50);
             Promise.resolve().then(() => document.title = 'promised');",
        );
        assert!(text(&s).contains("late x"), "{:?}", s.log());
        assert_eq!(s.title().as_deref(), Some("promised"));
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
