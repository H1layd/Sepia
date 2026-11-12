// Sepia's Web API, written over the engine's __dom_* natives.
// Anonymity: cookies in memory only, partitioned by site, storage in memory only, same-origin fetch only,
// and navigator/screen report the same values for everyone.
'use strict';
(() => {
  const G = globalThis;
  const wrappers = new Map();
  const log = (...a) => __log(a.map(v => {
    try { return typeof v === 'string' ? v : JSON.stringify(v); } catch (_) { return String(v); }
  }).join(' '));

  function wrap(id) {
    if (id === undefined || id === null || id < 0) return null;
    let w = wrappers.get(id);
    if (w) return w;
    const kind = __dom_kind(id);
    if (kind === 0) return null;
    w = kind === 3 ? new Text(id) : kind === 9 ? document : kind === 11 ? new DocumentFragment(id) : makeElement(id);
    wrappers.set(id, w);
    return w;
  }
  const idOf = n => (n && n.__id !== undefined ? n.__id : -1);
  const toNode = n => (n instanceof NodeBase ? n.__id : __dom_create_text(String(n)));
  const camelToKebab = k => k.replace(/[A-Z]/g, c => '-' + c.toLowerCase());
  const kebabToCamel = k => k.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

  // ----- events ---------------------------------------------------------
  class Event {
    constructor(type, init = {}) {
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = init.cancelable !== false;
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.timeStamp = 0;
      this.isTrusted = false;
      this.__stop = false;
      this.__stopNow = false;
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation() { this.__stop = true; }
    stopImmediatePropagation() { this.__stop = true; this.__stopNow = true; }
    composedPath() { return this.__path || []; }
  }
  class CustomEvent extends Event { constructor(t, i = {}) { super(t, i); this.detail = i.detail ?? null; } }
  class UIEvent extends Event {}
  class MouseEvent extends UIEvent {
    constructor(t, i = {}) { super(t, i); this.button = 0; this.clientX = 0; this.clientY = 0; this.altKey = !!i.altKey; this.ctrlKey = !!i.ctrlKey; this.metaKey = !!i.metaKey; this.shiftKey = !!i.shiftKey; }
  }
  class PointerEvent extends MouseEvent {}
  class KeyboardEvent extends UIEvent {
    constructor(t, i = {}) { super(t, i); this.key = i.key || ''; this.code = i.code || ''; this.altKey = !!i.altKey; this.ctrlKey = !!i.ctrlKey; this.metaKey = !!i.metaKey; this.shiftKey = !!i.shiftKey; this.repeat = false; }
  }
  class InputEvent extends UIEvent { constructor(t, i = {}) { super(t, i); this.data = i.data ?? null; this.inputType = i.inputType || 'insertText'; } }
  class FocusEvent extends UIEvent {}
  class SubmitEvent extends Event {}

  class EventTarget {
    constructor() { Object.defineProperty(this, '__l', { value: {}, writable: true }); }
    addEventListener(type, fn, opts) {
      if (!fn) return;
      const once = typeof opts === 'object' && opts && opts.once;
      (this.__l[type] ||= []).push({ fn, once });
    }
    removeEventListener(type, fn) {
      const list = this.__l[type];
      if (!list) return;
      const i = list.findIndex(l => l.fn === fn);
      if (i >= 0) list.splice(i, 1);
    }
    __fire(e) {
      e.currentTarget = this;
      const handler = this['on' + e.type];
      const list = (this.__l[e.type] || []).slice();
      if (typeof handler === 'function') list.unshift({ fn: handler });
      for (const l of list) {
        try {
          if (typeof l.fn === 'function') l.fn.call(this, e); else if (l.fn && l.fn.handleEvent) l.fn.handleEvent(e);
        } catch (err) { log('listener error:', String(err && err.stack || err)); }
        if (l.once) this.removeEventListener(e.type, l.fn);
        if (e.__stopNow) break;
      }
    }
    dispatchEvent(e) {
      e.target = this;
      // target, its ancestors, the document, the window
      const path = [];
      let n = this;
      while (n) {
        path.push(n);
        if (n === G) break;
        n = n === document ? G : n.parentNode;
      }
      e.__path = path;
      for (const t of path) {
        t.__fire(e);
        if (!e.bubbles || e.__stop) break;
      }
      return !e.defaultPrevented;
    }
  }

  // ----- nodes ----------------------------------------------------------
  class NodeBase extends EventTarget {
    constructor(id) { super(); Object.defineProperty(this, '__id', { value: id }); }
    get nodeType() { return __dom_kind(this.__id); }
    get nodeName() { const t = __dom_tag(this.__id); return t.startsWith('#') ? t : t.toUpperCase(); }
    get parentNode() { return wrap(__dom_parent(this.__id)); }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
    get childNodes() { return __dom_children(this.__id).map(wrap); }
    get firstChild() { return this.childNodes[0] || null; }
    get lastChild() { const c = this.childNodes; return c[c.length - 1] || null; }
    get nextSibling() { const p = this.parentNode; if (!p) return null; const c = p.childNodes; return c[c.indexOf(this) + 1] || null; }
    get previousSibling() { const p = this.parentNode; if (!p) return null; const c = p.childNodes; return c[c.indexOf(this) - 1] || null; }
    get isConnected() { for (let n = this; n; n = n.parentNode) if (n === document) return true; return false; }
    get ownerDocument() { return document; }
    get textContent() { return __dom_text(this.__id); }
    set textContent(v) { __dom_set_text(this.__id, v == null ? '' : String(v)); }
    appendChild(c) { __dom_insert(this.__id, toNode(c), -1); return c; }
    insertBefore(c, ref) { __dom_insert(this.__id, toNode(c), idOf(ref)); return c; }
    removeChild(c) { __dom_remove(c.__id); return c; }
    replaceChild(n, old) { __dom_insert(this.__id, toNode(n), old.__id); __dom_remove(old.__id); return old; }
    append(...cs) { for (const c of cs) __dom_insert(this.__id, toNode(c), -1); }
    prepend(...cs) { const first = idOf(this.firstChild); for (const c of cs) __dom_insert(this.__id, toNode(c), first); }
    replaceChildren(...cs) { __dom_clear(this.__id); this.append(...cs); }
    remove() { __dom_remove(this.__id); }
    before(...cs) { const p = this.parentNode; if (p) for (const c of cs) __dom_insert(p.__id, toNode(c), this.__id); }
    after(...cs) { const p = this.parentNode; if (!p) return; const next = idOf(this.nextSibling); for (const c of cs) __dom_insert(p.__id, toNode(c), next); }
    replaceWith(...cs) { this.before(...cs); this.remove(); }
    contains(o) { for (let n = o; n; n = n.parentNode) if (n === this) return true; return false; }
    hasChildNodes() { return __dom_children(this.__id).length > 0; }
    cloneNode(deep) { const html = __dom_html(this.__id, true); const tmp = __dom_create('div'); __dom_set_html(tmp, html); const c = __dom_children(tmp)[0]; if (!deep && c !== undefined) __dom_clear(c); return wrap(c); }
    isSameNode(o) { return o === this; }
    getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
  }
  class Text extends NodeBase {
    get data() { return this.textContent; }
    set data(v) { this.textContent = v; }
    get nodeValue() { return this.textContent; }
    get length() { return this.textContent.length; }
  }

  class ClassList {
    constructor(el) { this.el = el; }
    get __list() { return (this.el.getAttribute('class') || '').split(/\s+/).filter(Boolean); }
    __set(list) { this.el.setAttribute('class', list.join(' ')); }
    add(...c) { const l = this.__list; for (const x of c) if (!l.includes(x)) l.push(x); this.__set(l); }
    remove(...c) { this.__set(this.__list.filter(x => !c.includes(x))); }
    contains(c) { return this.__list.includes(c); }
    toggle(c, force) { const has = this.contains(c); const want = force === undefined ? !has : !!force; if (want && !has) this.add(c); if (!want && has) this.remove(c); return want; }
    replace(a, b) { const l = this.__list; const i = l.indexOf(a); if (i < 0) return false; l[i] = b; this.__set(l); return true; }
    item(i) { return this.__list[i] ?? null; }
    get length() { return this.__list.length; }
    get value() { return this.el.getAttribute('class') || ''; }
    forEach(f) { this.__list.forEach(f); }
    [Symbol.iterator]() { return this.__list[Symbol.iterator](); }
    toString() { return this.value; }
  }

  function styleObject(el) {
    const read = () => {
      const out = {};
      for (const part of (el.getAttribute('style') || '').split(';')) {
        const i = part.indexOf(':');
        if (i > 0) out[part.slice(0, i).trim()] = part.slice(i + 1).trim();
      }
      return out;
    };
    const write = o => el.setAttribute('style', Object.entries(o).filter(([, v]) => v !== '' && v != null).map(([k, v]) => `${k}: ${v}`).join('; '));
    const api = {
      setProperty(k, v) { const o = read(); o[k] = v; write(o); },
      removeProperty(k) { const o = read(); const v = o[k]; delete o[k]; write(o); return v || ''; },
      getPropertyValue(k) { return read()[k] || ''; },
    };
    return new Proxy(api, {
      get(t, k) {
        if (k in t) return t[k];
        if (k === 'cssText') return el.getAttribute('style') || '';
        return typeof k === 'string' ? (read()[camelToKebab(k)] || '') : undefined;
      },
      set(t, k, v) {
        if (k === 'cssText') { el.setAttribute('style', String(v)); return true; }
        const o = read(); o[camelToKebab(String(k))] = v == null ? '' : String(v); write(o); return true;
      },
    });
  }

  const reflected = ['href', 'src', 'alt', 'title', 'lang', 'name', 'type', 'placeholder', 'content', 'rel', 'target', 'action', 'method', 'role', 'for', 'dir', 'download'];
  const booleans = ['hidden', 'disabled', 'checked', 'selected', 'required', 'readonly', 'multiple', 'open', 'autofocus'];

  class Element extends NodeBase {
    get tagName() { return __dom_tag(this.__id).toUpperCase(); }
    get localName() { return __dom_tag(this.__id); }
    get id() { return this.getAttribute('id') || ''; }
    set id(v) { this.setAttribute('id', v); }
    get className() { return this.getAttribute('class') || ''; }
    set className(v) { this.setAttribute('class', v); }
    get classList() { return new ClassList(this); }
    get style() { return styleObject(this); }
    get dataset() {
      const el = this;
      return new Proxy({}, {
        get(_, k) { if (typeof k !== 'string') return undefined; const v = el.getAttribute('data-' + camelToKebab(k)); return v === null ? undefined : v; },
        set(_, k, v) { el.setAttribute('data-' + camelToKebab(String(k)), String(v)); return true; },
        deleteProperty(_, k) { el.removeAttribute('data-' + camelToKebab(String(k))); return true; },
        has(_, k) { return el.hasAttribute('data-' + camelToKebab(String(k))); },
        ownKeys() { return __dom_attr_names(el.__id).filter(n => n.startsWith('data-') && n !== 'data-sepia-id').map(n => kebabToCamel(n.slice(5))); },
        getOwnPropertyDescriptor(_, k) { const v = el.getAttribute('data-' + camelToKebab(String(k))); return v === null ? undefined : { value: v, enumerable: true, configurable: true }; },
      });
    }
    get attributes() { return __dom_attr_names(this.__id).map(name => ({ name, value: this.getAttribute(name) })); }
    getAttribute(n) { return __dom_get_attr(this.__id, String(n)); }
    setAttribute(n, v) { __dom_set_attr(this.__id, String(n), String(v)); }
    removeAttribute(n) { __dom_remove_attr(this.__id, String(n)); }
    hasAttribute(n) { return __dom_get_attr(this.__id, String(n)) !== null; }
    toggleAttribute(n, force) { const has = this.hasAttribute(n); const want = force === undefined ? !has : !!force; if (want) this.setAttribute(n, ''); else this.removeAttribute(n); return want; }
    getAttributeNames() { return __dom_attr_names(this.__id); }
    get children() { return this.childNodes.filter(n => n.nodeType === 1); }
    get childElementCount() { return this.children.length; }
    get firstElementChild() { return this.children[0] || null; }
    get lastElementChild() { const c = this.children; return c[c.length - 1] || null; }
    get nextElementSibling() { const p = this.parentNode; if (!p) return null; const c = p.children; return c[c.indexOf(this) + 1] || null; }
    get previousElementSibling() { const p = this.parentNode; if (!p) return null; const c = p.children; return c[c.indexOf(this) - 1] || null; }
    get innerHTML() { return __dom_html(this.__id, false); }
    set innerHTML(v) { __dom_set_html(this.__id, String(v)); }
    get outerHTML() { return __dom_html(this.__id, true); }
    get innerText() { return this.textContent; }
    set innerText(v) { this.textContent = v; }
    insertAdjacentHTML(pos, html) {
      const tmp = __dom_create('div');
      __dom_set_html(tmp, String(html));
      const nodes = __dom_children(tmp);
      const p = this.parentNode;
      for (const n of nodes) {
        switch (String(pos).toLowerCase()) {
          case 'beforebegin': if (p) __dom_insert(p.__id, n, this.__id); break;
          case 'afterbegin': __dom_insert(this.__id, n, idOf(this.firstChild)); break;
          case 'afterend': if (p) __dom_insert(p.__id, n, idOf(this.nextSibling)); break;
          default: __dom_insert(this.__id, n, -1);
        }
      }
    }
    insertAdjacentElement(pos, el) { this.insertAdjacentHTML(pos, ''); if (pos === 'beforeend') this.append(el); else if (pos === 'afterbegin') this.prepend(el); else if (pos === 'beforebegin') this.before(el); else this.after(el); return el; }
    insertAdjacentText(pos, text) { this.insertAdjacentElement(pos, document.createTextNode(text)); }
    querySelector(s) { return wrap(__dom_query(this.__id, String(s), false)[0] ?? -1); }
    querySelectorAll(s) { return nodeList(__dom_query(this.__id, String(s), true).map(wrap)); }
    getElementsByTagName(t) { return nodeList(__dom_query(this.__id, String(t), true).map(wrap)); }
    getElementsByClassName(c) { return nodeList(__dom_query(this.__id, String(c).split(/\s+/).filter(Boolean).map(x => '.' + x).join(''), true).map(wrap)); }
    matches(s) { return __dom_matches(this.__id, String(s)); }
    closest(s) { for (let e = this; e && e.nodeType === 1; e = e.parentElement) if (e.matches(s)) return e; return null; }
    focus() { __dom_focus(this.__id); this.dispatchEvent(new FocusEvent('focus')); }
    blur() {}
    click() { this.dispatchEvent(new MouseEvent('click', { bubbles: true })); }
    scrollIntoView() {}
    scrollTo() {}
    getBoundingClientRect() { return { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 }; }
    getClientRects() { return []; }
    get offsetWidth() { return 0; } get offsetHeight() { return 0; } get clientWidth() { return 0; } get clientHeight() { return 0; }
    get scrollTop() { return 0; } set scrollTop(_) {} get scrollLeft() { return 0; } set scrollLeft(_) {}
    get tabIndex() { const v = this.getAttribute('tabindex'); return v === null ? -1 : Number(v); }
    set tabIndex(v) { this.setAttribute('tabindex', String(v)); }
    get value() { const v = this.getAttribute('value'); if (v !== null) return v; return this.localName === 'textarea' ? this.textContent : this.localName === 'select' ? selectValue(this) : ''; }
    set value(v) { this.setAttribute('value', v == null ? '' : String(v)); }
    get form() { return this.closest('form'); }
    animate() { return { finished: Promise.resolve(), cancel() {}, onfinish: null }; }
    attachShadow() { return this; }
    get shadowRoot() { return null; }
  }
  for (const name of reflected) {
    Object.defineProperty(Element.prototype, name, {
      get() { const v = this.getAttribute(name); if (name === 'href' && v !== null) { try { return new URL(v, location.href).href; } catch (_) { return v; } } return v ?? ''; },
      set(v) { this.setAttribute(name, v); },
      configurable: true,
    });
  }
  Object.defineProperty(Element.prototype, 'htmlFor', { get() { return this.getAttribute('for') || ''; }, set(v) { this.setAttribute('for', v); } });
  for (const name of booleans) {
    Object.defineProperty(Element.prototype, name, {
      get() { return this.hasAttribute(name); },
      set(v) { if (v) this.setAttribute(name, ''); else this.removeAttribute(name); },
      configurable: true,
    });
  }
  function selectValue(sel) { const opts = sel.querySelectorAll('option'); const chosen = opts.find(o => o.selected) || opts[0]; return chosen ? (chosen.getAttribute('value') ?? chosen.textContent) : ''; }
  function nodeList(arr) { arr.item = i => arr[i] ?? null; return arr; }

  // element classes, for instanceof checks
  const HTMLElement = class HTMLElement extends Element {};
  const classes = {};
  for (const n of ['Div', 'Span', 'Anchor', 'Button', 'Input', 'TextArea', 'Select', 'Option', 'Form', 'Image', 'Paragraph', 'Heading', 'UList', 'OList', 'LI', 'Table', 'TableRow', 'TableCell', 'Label', 'Template', 'Canvas', 'Script', 'Style', 'Link', 'Meta', 'Body', 'Html', 'Head', 'Pre', 'Video', 'Audio', 'Iframe', 'Dialog', 'Details', 'Section']) {
    classes[n] = { [`HTML${n}Element`]: class extends HTMLElement {} }[`HTML${n}Element`];
  }
  const tagClass = { div: 'Div', span: 'Span', a: 'Anchor', button: 'Button', input: 'Input', textarea: 'TextArea', select: 'Select', option: 'Option', form: 'Form', img: 'Image', p: 'Paragraph', h1: 'Heading', h2: 'Heading', h3: 'Heading', h4: 'Heading', h5: 'Heading', h6: 'Heading', ul: 'UList', ol: 'OList', li: 'LI', table: 'Table', tr: 'TableRow', td: 'TableCell', th: 'TableCell', label: 'Label', template: 'Template', canvas: 'Canvas', script: 'Script', style: 'Style', link: 'Link', meta: 'Meta', body: 'Body', html: 'Html', head: 'Head', pre: 'Pre', video: 'Video', audio: 'Audio', iframe: 'Iframe', dialog: 'Dialog', details: 'Details', section: 'Section' };
  function makeElement(id) { const C = classes[tagClass[__dom_tag(id)]] || HTMLElement; return new C(id); }
  // a template's content is its children
  Object.defineProperty(classes.Template.prototype, 'content', { get() { const f = document.createDocumentFragment(); for (const c of this.childNodes) f.append(c.cloneNode(true)); return f; } });
  // forms submit nowhere from script; the window handles real submits
  classes.Form.prototype.submit = function () {};
  classes.Form.prototype.reset = function () {};
  classes.Form.prototype.requestSubmit = function () { this.dispatchEvent(new SubmitEvent('submit', { bubbles: true })); };
  classes.Dialog.prototype.showModal = function () { this.setAttribute('open', ''); };
  classes.Dialog.prototype.show = function () { this.setAttribute('open', ''); };
  classes.Dialog.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new Event('close')); };
  // no canvas: it would be a fingerprinting surface
  classes.Canvas.prototype.getContext = function () { return null; };
  classes.Canvas.prototype.toDataURL = function () { return 'data:,'; };

  class DocumentFragment extends NodeBase {
    querySelector(s) { return Element.prototype.querySelector.call(this, s); }
    querySelectorAll(s) { return Element.prototype.querySelectorAll.call(this, s); }
    get children() { return this.childNodes.filter(n => n.nodeType === 1); }
  }

  class Document extends NodeBase {
    get documentElement() { return wrap(__dom_find_tag('html')); }
    get head() { return wrap(__dom_find_tag('head')); }
    get body() { return wrap(__dom_find_tag('body')); }
    get title() { return __dom_title(); }
    set title(v) { __dom_set_title(String(v)); }
    get cookie() { return __cookie_get(); }
    set cookie(v) { __cookie_set(String(v)); }
    get referrer() { return ''; }
    get readyState() { return G.__readyState || 'loading'; }
    get visibilityState() { return 'visible'; }
    get hidden() { return false; }
    get URL() { return location.href; }
    get location() { return location; }
    get defaultView() { return G; }
    get activeElement() { return this.body; }
    get characterSet() { return 'UTF-8'; }
    get compatMode() { return 'CSS1Compat'; }
    get scrollingElement() { return this.documentElement; }
    get fonts() { return { ready: Promise.resolve(), load: () => Promise.resolve([]), check: () => true, add() {}, forEach() {} }; }
    createElement(t) { return wrap(__dom_create(String(t))); }
    createElementNS(_, t) { return this.createElement(t); }
    createTextNode(t) { return wrap(__dom_create_text(String(t))); }
    createComment() { return wrap(__dom_create_text('')); }
    createDocumentFragment() { return wrap(__dom_create('#fragment')); }
    createEvent() { return new Event(''); }
    getElementById(id) { return wrap(__dom_query(0, '#' + CSS.escape(String(id)), false)[0] ?? -1); }
    getElementsByName(n) { return this.querySelectorAll(`[name="${n}"]`); }
    querySelector(s) { return wrap(__dom_query(0, String(s), false)[0] ?? -1); }
    querySelectorAll(s) { return nodeList(__dom_query(0, String(s), true).map(wrap)); }
    getElementsByTagName(t) { return this.querySelectorAll(t); }
    getElementsByClassName(c) { return Element.prototype.getElementsByClassName.call({ __id: 0 }, c); }
    hasFocus() { return true; }
    execCommand() { return false; }
  }

  // ----- the window ------------------------------------------------------
  const document = Object.create(Document.prototype);
  Object.defineProperty(document, '__id', { value: 0 });
  Object.defineProperty(document, '__l', { value: {}, writable: true });
  wrappers.set(0, document);

  for (const k of ['addEventListener', 'removeEventListener', '__fire', 'dispatchEvent']) G[k] = EventTarget.prototype[k];
  Object.defineProperty(G, '__l', { value: {}, writable: true });

  const parts = () => {
    const href = __page_url();
    const m = href.match(/^([a-z]+:)\/\/([^/:?#]+)(?::(\d+))?([^?#]*)(\?[^#]*)?(#.*)?$/i) || [];
    return { href, protocol: m[1] || '', hostname: m[2] || '', port: m[3] || '', pathname: m[4] || '/', search: m[5] || '', hash: m[6] && m[6] !== '#' ? m[6] : '' };
  };
  const location = {
    get href() { return parts().href; }, set href(v) { this.assign(v); },
    get protocol() { return parts().protocol; },
    get host() { const p = parts(); return p.hostname + (p.port ? ':' + p.port : ''); },
    get hostname() { return parts().hostname; },
    get port() { return parts().port; },
    get pathname() { return parts().pathname; },
    get search() { return parts().search; },
    get hash() { return parts().hash; },
    set hash(v) { const h = String(v).replace(/^#/, ''); const old = this.href; __page_set_url('#' + h); if (this.href !== old) G.dispatchEvent(new Event('hashchange')); },
    get origin() { const p = parts(); return `${p.protocol}//${this.host}`; },
    assign(u) { const s = String(u); if (s.startsWith('#')) this.hash = s; else __page_navigate(s); },
    replace(u) { this.assign(u); },
    reload() { __page_navigate(this.href); },
    toString() { return this.href; },
  };
  const history = {
    length: 1, state: null, scrollRestoration: 'auto',
    pushState(state, _, url) { this.state = state ?? null; if (url != null) __page_set_url(String(url)); },
    replaceState(state, _, url) { this.state = state ?? null; if (url != null) __page_set_url(String(url)); },
    back() {}, forward() {}, go() {},
  };

  class Storage {
    constructor() { Object.defineProperty(this, '__m', { value: new Map() }); }
    getItem(k) { const v = this.__m.get(String(k)); return v === undefined ? null : v; }
    setItem(k, v) { this.__m.set(String(k), String(v)); }
    removeItem(k) { this.__m.delete(String(k)); }
    clear() { this.__m.clear(); }
    key(i) { return [...this.__m.keys()][i] ?? null; }
    get length() { return this.__m.size; }
  }

  // timers: run in order when the page settles (no real waiting)
  let timerId = 1;
  const timers = new Map();
  G.setTimeout = (fn, ms = 0, ...args) => { const id = timerId++; timers.set(id, { fn, args, ms: Number(ms) || 0, repeat: false }); return id; };
  G.setInterval = (fn, ms = 0, ...args) => { const id = timerId++; timers.set(id, { fn, args, ms: Number(ms) || 0, repeat: true, runs: 0 }); return id; };
  G.clearTimeout = G.clearInterval = id => { timers.delete(id); };
  G.requestAnimationFrame = fn => G.setTimeout(() => fn(G.performance.now()), 16);
  G.cancelAnimationFrame = G.clearTimeout;
  G.requestIdleCallback = fn => G.setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1);
  G.cancelIdleCallback = G.clearTimeout;
  G.queueMicrotask = fn => Promise.resolve().then(fn);
  G.__sepia_tick = () => {
    const due = [...timers.entries()].sort((a, b) => a[1].ms - b[1].ms);
    let ran = 0;
    for (const [id, t] of due) {
      if (!timers.has(id)) continue;
      // intervals tick a few times, then stop: nothing animates here
      if (t.repeat) { if (++t.runs > 3) { timers.delete(id); continue; } } else timers.delete(id);
      try { typeof t.fn === 'function' ? t.fn(...t.args) : (0, eval)(String(t.fn)); } catch (e) { log('timer error:', String(e && e.stack || e)); }
      ran++;
    }
    return ran;
  };

  // ----- small Web APIs ---------------------------------------------------
  class URLSearchParams {
    constructor(init = '') {
      this.__p = [];
      if (typeof init === 'string') {
        for (const part of init.replace(/^\?/, '').split('&').filter(Boolean)) {
          const [k, v = ''] = part.split('=');
          this.__p.push([decodeURIComponent(k.replace(/\+/g, ' ')), decodeURIComponent(v.replace(/\+/g, ' '))]);
        }
      } else if (init && typeof init === 'object') {
        for (const [k, v] of (Symbol.iterator in init ? init : Object.entries(init))) this.__p.push([String(k), String(v)]);
      }
    }
    get(k) { const e = this.__p.find(p => p[0] === k); return e ? e[1] : null; }
    getAll(k) { return this.__p.filter(p => p[0] === k).map(p => p[1]); }
    has(k) { return this.__p.some(p => p[0] === k); }
    set(k, v) { this.delete(k); this.__p.push([k, String(v)]); }
    append(k, v) { this.__p.push([k, String(v)]); }
    delete(k) { this.__p = this.__p.filter(p => p[0] !== k); }
    forEach(f) { this.__p.forEach(([k, v]) => f(v, k, this)); }
    entries() { return this.__p[Symbol.iterator](); }
    keys() { return this.__p.map(p => p[0])[Symbol.iterator](); }
    values() { return this.__p.map(p => p[1])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return this.__p.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join('&'); }
  }
  class URL {
    constructor(u, base) {
      u = String(u);
      let m = u.match(/^([a-z][a-z0-9+.-]*:)(\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/i);
      if (!m) {
        if (base === undefined) throw new TypeError('Invalid URL: ' + u);
        const b = new URL(base);
        if (u.startsWith('//')) return new URL(b.protocol + u);
        if (u.startsWith('#')) return new URL(b.origin + b.pathname + b.search + u);
        if (u.startsWith('?')) return new URL(b.origin + b.pathname + u);
        let path = u.startsWith('/') ? u : b.pathname.replace(/[^/]*$/, '') + u;
        const [p, rest = ''] = path.split(/(?=[?#])/);
        const segs = [];
        for (const s of p.split('/')) { if (s === '..') segs.pop(); else if (s !== '.') segs.push(s); }
        return new URL(b.origin + segs.join('/').replace(/^(?!\/)/, '/') + rest + (path.includes('?') || path.includes('#') ? path.slice(p.length + rest.length) : ''));
      }
      const auth = m[3] || '';
      const [hostname, port = ''] = auth.replace(/^.*@/, '').split(':');
      Object.assign(this, { protocol: m[1].toLowerCase(), hostname: hostname.toLowerCase(), port, pathname: m[4] || (m[2] ? '/' : ''), search: m[5] && m[5] !== '?' ? m[5] : '', hash: m[6] && m[6] !== '#' ? m[6] : '' });
      this.searchParams = new URLSearchParams(this.search);
    }
    get host() { return this.hostname + (this.port ? ':' + this.port : ''); }
    get origin() { return `${this.protocol}//${this.host}`; }
    get href() { const q = this.searchParams.toString(); return `${this.origin}${this.pathname}${q ? '?' + q : ''}${this.hash}`; }
    toString() { return this.href; }
    toJSON() { return this.href; }
    static canParse(u, b) { try { new URL(u, b); return true; } catch (_) { return false; } }
  }

  class Headers {
    constructor(init) { this.__h = new Map(); if (init) for (const [k, v] of Object.entries(init)) this.__h.set(k.toLowerCase(), String(v)); }
    get(k) { return this.__h.get(String(k).toLowerCase()) ?? null; }
    set(k, v) { this.__h.set(String(k).toLowerCase(), String(v)); }
    has(k) { return this.__h.has(String(k).toLowerCase()); }
    forEach(f) { this.__h.forEach((v, k) => f(v, k)); }
  }
  class Response {
    constructor(body, init = {}) { this.__body = body ?? ''; this.status = init.status ?? 200; this.ok = this.status >= 200 && this.status < 300; this.url = init.url || ''; this.headers = new Headers(); this.statusText = ''; this.redirected = false; this.type = 'basic'; }
    text() { return Promise.resolve(this.__body); }
    json() { return Promise.resolve().then(() => JSON.parse(this.__body)); }
    clone() { return new Response(this.__body, { status: this.status, url: this.url }); }
  }
  // the page's own site only, through the page's chain (enforced natively);
  // custom request headers are dropped, only Content-Type goes out
  function bodyOf(b, headers) {
    let type = headers && (headers.get ? headers.get('content-type') : (headers['Content-Type'] || headers['content-type']));
    if (b == null) return [undefined, undefined];
    if (b instanceof URLSearchParams) return [b.toString(), type || 'application/x-www-form-urlencoded;charset=UTF-8'];
    if (typeof FormData !== 'undefined' && b instanceof FormData) return [new URLSearchParams(b.__pairs).toString(), 'application/x-www-form-urlencoded;charset=UTF-8'];
    if (typeof b === 'object' && !(typeof b === 'string')) return [JSON.stringify(b), type || 'application/json'];
    return [String(b), type || 'text/plain;charset=UTF-8'];
  }
  function sendNow(url, method, body, headers) {
    const [data, type] = bodyOf(body, headers);
    return __fetch(String(url), method ? String(method) : (data === undefined ? 'GET' : 'POST'), data, type);
  }
  G.fetch = (input, opts = {}) => new Promise((resolve, reject) => {
    try {
      const url = typeof input === 'string' ? input : input && (input.href || input.url);
      const req = input && typeof input === 'object' && input.url ? input : {};
      const [status, body, finalUrl, type] = sendNow(url, opts.method || req.method, opts.body ?? req.body, opts.headers || req.headers);
      const response = new Response(body, { status, url: finalUrl });
      if (type) response.headers.set('content-type', type);
      resolve(response);
    } catch (e) { reject(e instanceof Error ? e : new TypeError(String(e))); }
  });
  G.Request = class Request { constructor(u, o = {}) { this.url = String(u && u.url || u); this.method = o.method || 'GET'; this.body = o.body; this.headers = o.headers; } };
  G.FormData = class FormData {
    constructor(form) {
      this.__pairs = [];
      if (form && form.querySelectorAll) for (const f of form.querySelectorAll('input, select, textarea')) {
        if (!f.name || f.disabled || ((f.type === 'checkbox' || f.type === 'radio') && !f.checked) || ['submit', 'button', 'reset', 'file', 'image'].includes(f.type)) continue;
        this.__pairs.push([f.name, f.value]);
      }
    }
    append(k, v) { this.__pairs.push([String(k), String(v)]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) { const p = this.__pairs.find(p => p[0] === k); return p ? p[1] : null; }
    getAll(k) { return this.__pairs.filter(p => p[0] === k).map(p => p[1]); }
    has(k) { return this.__pairs.some(p => p[0] === k); }
    delete(k) { this.__pairs = this.__pairs.filter(p => p[0] !== k); }
    entries() { return this.__pairs[Symbol.iterator](); }
    [Symbol.iterator]() { return this.__pairs[Symbol.iterator](); }
    forEach(f) { this.__pairs.forEach(([k, v]) => f(v, k, this)); }
  };
  // XMLHttpRequest over the same native call; events fire on the next tick
  G.XMLHttpRequest = class XMLHttpRequest extends EventTarget {
    constructor() { super(); this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = ''; this.response = ''; this.responseType = ''; this.responseURL = ''; this.__h = {}; this.__rh = ''; this.timeout = 0; this.withCredentials = false; this.upload = new EventTarget(); }
    open(method, url) { this.__m = method; this.__u = url; this.readyState = 1; }
    setRequestHeader(k, v) { this.__h[k] = v; }
    getResponseHeader(k) { return String(k).toLowerCase() === 'content-type' ? (this.__rh || null) : null; }
    getAllResponseHeaders() { return this.__rh ? 'content-type: ' + this.__rh + '\r\n' : ''; }
    overrideMimeType() {}
    abort() { this.__aborted = true; }
    send(body) {
      setTimeout(() => {
        if (this.__aborted) return;
        const fire = (type) => { const e = new Event(type); this.dispatchEvent(e); const h = this['on' + type]; if (typeof h === 'function') h.call(this, e); };
        try {
          const [status, text, finalUrl, type] = sendNow(this.__u, this.__m, body, this.__h);
          Object.assign(this, { status, responseText: text, responseURL: finalUrl, __rh: type, readyState: 4, statusText: status === 200 ? 'OK' : '' });
          this.response = this.responseType === 'json' ? (() => { try { return JSON.parse(text); } catch (_) { return null; } })() : text;
          fire('readystatechange'); fire('load');
        } catch (e) {
          this.readyState = 4; fire('readystatechange'); fire('error');
        }
        fire('loadend');
      }, 0);
    }
  };
  for (const [k, v] of Object.entries({ UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 })) G.XMLHttpRequest[k] = v;
  G.AbortController = class AbortController { constructor() { this.signal = { aborted: false, addEventListener() {}, removeEventListener() {} }; } abort() { this.signal.aborted = true; } };

  // Intl, the parts pages lean on (the engine is built without ICU data)
  if (typeof G.Intl === 'undefined') {
    const plural = (locale, n) => {
      const l = String(locale || 'en').slice(0, 2);
      const i = Math.floor(Math.abs(n));
      const int = Number.isInteger(n);
      if (l === 'ru' || l === 'uk') {
        if (!int) return 'other';
        if (i % 10 === 1 && i % 100 !== 11) return 'one';
        if (i % 10 >= 2 && i % 10 <= 4 && (i % 100 < 12 || i % 100 > 14)) return 'few';
        return 'many';
      }
      if (l === 'fr') return i === 0 || i === 1 ? 'one' : 'other';
      if (l === 'ja' || l === 'zh' || l === 'ko') return 'other';
      return n === 1 ? 'one' : 'other';
    };
    const group = (s, sep) => s.replace(/\B(?=(\d{3})+(?!\d))/g, sep);
    G.Intl = {
      PluralRules: class { constructor(l, o = {}) { this.l = l; this.o = o; } select(n) { return plural(this.l, Number(n)); } resolvedOptions() { return { locale: this.l || 'en' }; } },
      NumberFormat: class {
        constructor(l, o = {}) { this.l = String(l || 'en'); this.o = o; }
        format(n) {
          const max = this.o.maximumFractionDigits ?? 3;
          const min = this.o.minimumFractionDigits ?? 0;
          let s = Number(n).toFixed(max);
          if (s.includes('.')) { s = s.replace(/0+$/, ''); const frac = s.split('.')[1] || ''; if (frac.length < min) s += '0'.repeat(min - frac.length); s = s.replace(/\.$/, ''); }
          const [int, frac] = s.split('.');
          const comma = ['ru', 'de', 'fr', 'es', 'it', 'pl', 'uk'].includes(this.l.slice(0, 2));
          const sep = this.l.startsWith('de') ? '.' : comma ? ' ' : ',';
          return group(int, sep) + (frac ? (comma ? ',' : '.') + frac : '');
        }
        resolvedOptions() { return { locale: this.l }; }
      },
      DateTimeFormat: class { constructor(l, o = {}) { this.l = l; this.o = o; } format(d) { return new Date(d ?? Date.now()).toISOString().slice(0, 10); } resolvedOptions() { return { locale: this.l || 'en', timeZone: 'UTC' }; } },
      Collator: class { compare(a, b) { return String(a) < String(b) ? -1 : String(a) > String(b) ? 1 : 0; } },
      RelativeTimeFormat: class { format(n, u) { return `${n} ${u}`; } },
      ListFormat: class { format(list) { return [...list].join(', '); } },
      getCanonicalLocales: l => [].concat(l || []),
    };
    String.prototype.toLocaleLowerCase = String.prototype.toLowerCase;
    String.prototype.toLocaleUpperCase = String.prototype.toUpperCase;
    Number.prototype.toLocaleString = function (l, o) { return new G.Intl.NumberFormat(l, o).format(this); };
    Date.prototype.toLocaleDateString = function () { return this.toISOString().slice(0, 10); };
    Date.prototype.toLocaleTimeString = function () { return this.toISOString().slice(11, 19); };
    Date.prototype.toLocaleString = function () { return this.toISOString().replace('T', ' ').slice(0, 19); };
  }

  // stubs: observers never fire, media queries never match, nothing is measured
  const Observer = class { constructor(cb) { this.cb = cb; } observe() {} unobserve() {} disconnect() {} takeRecords() { return []; } };
  G.MutationObserver = Observer;
  G.ResizeObserver = Observer;
  G.PerformanceObserver = Observer;
  // lazy loaders: everything counts as visible at once
  G.IntersectionObserver = class extends Observer { observe(el) { const cb = this.cb; setTimeout(() => cb([{ target: el, isIntersecting: true, intersectionRatio: 1 }], this), 0); } };
  G.matchMedia = q => ({ matches: false, media: String(q), addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {}, onchange: null });
  G.getComputedStyle = el => el && el.style ? el.style : styleObject({ getAttribute: () => '', setAttribute() {} });
  G.CSS = { escape: s => String(s).replace(/([^\w-])/g, '\\$1'), supports: () => false };

  // the same answers for everybody (fingerprinting)
  G.navigator = {
    userAgent: 'sepia-browser/0.1', appName: 'Netscape', appVersion: '5.0', platform: '', vendor: '', product: 'Gecko',
    language: 'en-US', languages: ['en-US'], hardwareConcurrency: 2, deviceMemory: 4, maxTouchPoints: 0,
    cookieEnabled: true, onLine: true, doNotTrack: null, webdriver: false, pdfViewerEnabled: false,
    plugins: [], mimeTypes: [], clipboard: { writeText: () => Promise.reject(new Error('blocked')), readText: () => Promise.reject(new Error('blocked')) },
    sendBeacon: () => false, vibrate: () => false, share: () => Promise.reject(new Error('blocked')),
    serviceWorker: undefined, geolocation: undefined, mediaDevices: undefined,
  };
  G.screen = { width: 1280, height: 800, availWidth: 1280, availHeight: 800, colorDepth: 24, pixelDepth: 24, orientation: { type: 'landscape-primary', angle: 0 } };
  G.devicePixelRatio = 1;
  G.innerWidth = G.outerWidth = G.__viewport ? G.__viewport[0] : 1024;
  G.innerHeight = G.outerHeight = G.__viewport ? G.__viewport[1] : 800;
  G.scrollX = G.scrollY = G.pageXOffset = G.pageYOffset = 0;
  G.scrollTo = G.scroll = G.scrollBy = () => {};
  G.alert = G.print = () => {};
  G.confirm = () => false;
  G.prompt = () => null;
  G.open = () => null;
  G.close = () => {};
  G.focus = G.blur = () => {};
  // coarse time: 100 ms steps, like Tor Browser
  const start = Date.now();
  G.performance = { now: () => Math.floor((Date.now() - start) / 100) * 100, timeOrigin: Math.floor(start / 100) * 100, mark() {}, measure() {}, getEntriesByType: () => [], getEntriesByName: () => [] };
  G.crypto = { getRandomValues: a => { for (let i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 256); return a; }, randomUUID: () => 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, c => { const r = Math.random() * 16 | 0; return (c === 'x' ? r : (r & 3 | 8)).toString(16); }) };
  G.console = { log, info: log, warn: log, error: log, debug: () => {}, trace: () => {}, table: log, group: () => {}, groupEnd: () => {}, groupCollapsed: () => {}, time: () => {}, timeEnd: () => {}, assert: (c, ...a) => { if (!c) log('assert', ...a); }, dir: log, count: () => {} };
  G.structuredClone = v => JSON.parse(JSON.stringify(v));
  G.atob = G.atob || (s => { const t = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'; let o = '', b = 0, n = 0; for (const c of String(s).replace(/[^A-Za-z0-9+/]/g, '')) { b = (b << 6) | t.indexOf(c); n += 6; if (n >= 8) { n -= 8; o += String.fromCharCode((b >> n) & 255); } } return o; });
  G.btoa = G.btoa || (s => { const t = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'; let o = ''; s = String(s); for (let i = 0; i < s.length; i += 3) { const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2); o += t[a >> 2] + t[((a & 3) << 4) | (b >> 4 || 0)] + (isNaN(b) ? '=' : t[((b & 15) << 2) | (c >> 6 || 0)]) + (isNaN(c) ? '=' : t[c & 63]); } return o; });

  Object.assign(G, {
    window: G, self: G, top: G, parent: G, frames: G, globalThis: G,
    document, location, history,
    localStorage: new Storage(), sessionStorage: new Storage(),
    Event, CustomEvent, UIEvent, MouseEvent, PointerEvent, KeyboardEvent, InputEvent, FocusEvent, SubmitEvent, EventTarget,
    Node: NodeBase, Element, HTMLElement, Text, Document, DocumentFragment, URL, URLSearchParams, Response, Headers, Storage,
    ...classes,
  });
  G.Image = function Image(w, h) { const img = document.createElement('img'); if (w) img.setAttribute('width', w); if (h) img.setAttribute('height', h); return img; };

  // ----- what the window calls ---------------------------------------------
  G.__sepia_loaded = () => {
    G.__readyState = 'interactive';
    document.dispatchEvent(new Event('readystatechange'));
    document.dispatchEvent(new Event('DOMContentLoaded', { bubbles: true }));
    G.__readyState = 'complete';
    document.dispatchEvent(new Event('readystatechange'));
    G.dispatchEvent(new Event('load'));
    G.dispatchEvent(new Event('pageshow'));
  };
  G.__sepia_click = (id, href) => {
    const el = wrap(id);
    if (!el) { if (href) __page_navigate(href); return; }
    for (const type of ['pointerdown', 'mousedown', 'pointerup', 'mouseup']) el.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    const go = el.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    if (!go) return;
    // default actions
    const button = el.closest('button, input[type=submit]');
    const form = button && button.closest('form');
    if (form && (button.getAttribute('type') || 'submit') === 'submit') {
      const ok = form.dispatchEvent(new SubmitEvent('submit', { bubbles: true }));
      if (ok) submitForm(form);
      return;
    }
    const box = el.closest('input[type=checkbox], input[type=radio]');
    if (box) { box.checked = box.type === 'radio' ? true : !box.checked; box.dispatchEvent(new Event('input', { bubbles: true })); box.dispatchEvent(new Event('change', { bubbles: true })); return; }
    const field = el.closest('input, textarea, select');
    if (field) { field.focus(); return; }
    if (href) {
      if (href.startsWith('#')) location.hash = href; else __page_navigate(href);
    }
  };
  function submitForm(form) {
    const method = (form.getAttribute('method') || 'get').toLowerCase();
    const action = form.getAttribute('action') || location.pathname + location.search;
    if (method === 'post') __page_navigate(action, formBody(form));
    else __page_navigate(formUrl(form));
  }
  function formBody(form) {
    const params = new URLSearchParams();
    for (const f of form.querySelectorAll('input, select, textarea')) {
      if (!f.name || f.disabled) continue;
      if ((f.type === 'checkbox' || f.type === 'radio') && !f.checked) continue;
      if (['submit', 'button', 'reset', 'file', 'image'].includes(f.type)) continue;
      params.append(f.name, f.value);
    }
    return params.toString();
  }
  function formUrl(form) {
    const action = form.getAttribute('action') || location.pathname;
    return action.split('?')[0] + '?' + formBody(form);
  }
  G.__sepia_input = (id, value) => {
    const el = wrap(id);
    if (!el) return;
    el.value = value;
    el.dispatchEvent(new InputEvent('input', { bubbles: true, data: value }));
  };
  G.__sepia_key = (id, key) => {
    const el = wrap(id) || document.body || document;
    const down = el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key }));
    el.dispatchEvent(new KeyboardEvent('keyup', { bubbles: true, key }));
    if (down && key === 'Enter' && el.closest && el.closest('form') && el.localName === 'input') {
      const form = el.closest('form');
      if (form.dispatchEvent(new SubmitEvent('submit', { bubbles: true }))) submitForm(form);
      return;
    }
    if (down && key === 'Enter' && el.dispatchEvent) el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  G.__sepia_hash = hash => { location.hash = hash; };
})();
