// Sepia's Web API, written over the engine's __dom_* natives.
// Anonymity: cookies in memory only, partitioned by site, storage in memory only, same-origin fetch only,
// and navigator/screen report the same values for everyone.
'use strict';
(() => {
  const G = globalThis;
  // who the page sees: the browser and country chosen in the settings (persona.rs)
  const persona = G.__persona || { userAgent: 'sepia-browser/0.1', appVersion: '5.0', platform: '', vendor: '', productSub: '20100101', oscpu: null, hints: null, language: 'en-US', languages: ['en-US'], timeZone: 'UTC' };
  const wrappers = new Map();
  // custom elements: name → class, class → name, the node being upgraded
  const defined = new Map(), names = new Map();
  let upgrading;
  const nodeHandle = Symbol('internal node handle');
  const log = (...a) => __log(a.map(v => {
    try { return typeof v === 'string' ? v : v instanceof Error ? `${v}\n${v.stack || ''}` : JSON.stringify(v); } catch (_) { return String(v); }
  }).join(' '));

  function wrap(id) {
    if (id === undefined || id === null || id < 0) return null;
    let w = wrappers.get(id);
    if (w) return w;
    const kind = __dom_kind(id);
    if (kind === 0) return null;
    w = kind === 3 ? new Text(id, nodeHandle) : kind === 8 ? new Comment(id, nodeHandle) : kind === 9 ? document : kind === 11 ? (__dom_shadow(id) >= 0 ? new ShadowRoot(id) : new DocumentFragment(id)) : makeElement(id);
    wrappers.set(id, w);
    return w;
  }
  const idOf = n => (n && n.__sepiaNode !== undefined ? n.__sepiaNode : -1);
  // a node is a node by what it is, not by its prototype chain: libraries
  // re-prototype nodes (ShadyDOM turns a fragment into its ShadowRoot)
  const isNode = n => n !== null && typeof n === 'object' && typeof n.__sepiaNode === 'number' && wrappers.get(n.__sepiaNode) === n;
  const toNode = n => (isNode(n) ? n.__sepiaNode : __dom_create_text(String(n)));
  // The tree itself. The browser's own methods read it directly, never through
  // the public accessors: pages replace those (ShadyDOM swaps childNodes,
  // parentNode… for its logical tree) and the built-ins must keep working.
  const parentId = id => { const p = __dom_parent(id); return typeof p === 'number' && p >= 0 ? p : -1; };
  const parentOf = n => wrap(parentId(n.__sepiaNode));
  const kidsOf = n => __dom_children(n.__sepiaNode).map(wrap);
  const elementKids = n => __dom_children(n.__sepiaNode).filter(c => __dom_kind(c) === 1).map(wrap);
  const siblingOf = (n, step, elementsOnly) => {
    const p = parentId(n.__sepiaNode);
    if (p < 0) return null;
    let k = __dom_children(p);
    if (elementsOnly) k = k.filter(c => c === n.__sepiaNode || __dom_kind(c) === 1);
    const i = k.indexOf(n.__sepiaNode);
    return i < 0 ? null : wrap(k[i + step] ?? -1);
  };
  // up the tree, from a shadow root to its host (the "shadow-including" ancestors)
  const upId = id => { const p = parentId(id); if (p >= 0) return p; if (__dom_kind(id) === 11) { const h = __dom_shadow(id); if (typeof h === 'number' && h >= 0) return h; } return -1; };
  const isConnectedId = id => { for (let n = id; n >= 0; n = upId(n)) if (n === 0) return true; return false; };
  const containsId = (a, b) => { for (let n = b; n >= 0; n = parentId(n)) if (n === a) return true; return false; };
  const camelToKebab = k => k.replace(/[A-Z]/g, c => '-' + c.toLowerCase());
  const kebabToCamel = k => k.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

  // ----- events ---------------------------------------------------------
  class DOMException extends Error {
    constructor(message = '', name = 'Error') { super(String(message)); this.name = String(name); }
    get code() { return ({ IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
      InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
      NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13,
      NamespaceError: 14, InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18,
      NetworkError: 19, AbortError: 20, URLMismatchError: 21, QuotaExceededError: 22,
      TimeoutError: 23, InvalidNodeTypeError: 24, DataCloneError: 25 })[this.name] || 0; }
  }
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
    initEvent(type, bubbles = false, cancelable = false) {
      if (this.__dispatching) return;
      this.type = String(type);
      this.bubbles = !!bubbles;
      this.cancelable = !!cancelable;
      this.defaultPrevented = false;
      this.target = null;
      this.__stop = this.__stopNow = false;
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation() { this.__stop = true; }
    stopImmediatePropagation() { this.__stop = true; this.__stopNow = true; }
    composedPath() { return this.__path || []; }
  }
  class CustomEvent extends Event {
    constructor(t, i = {}) { super(t, i); this.detail = i.detail ?? null; }
    initCustomEvent(type, bubbles = false, cancelable = false, detail = null) {
      if (this.__dispatching) return;
      this.initEvent(type, bubbles, cancelable);
      this.detail = detail;
    }
  }
  class UIEvent extends Event {}
  class MouseEvent extends UIEvent {
    constructor(t, i = {}) {
      super(t, i);
      this.button = i.button ?? 0; this.buttons = i.buttons ?? 0; this.detail = i.detail ?? 0;
      this.clientX = this.x = this.pageX = this.offsetX = this.screenX = i.clientX ?? 0;
      this.clientY = this.y = this.pageY = this.offsetY = this.screenY = i.clientY ?? 0;
      this.movementX = this.movementY = 0; this.relatedTarget = i.relatedTarget ?? null;
      this.altKey = !!i.altKey; this.ctrlKey = !!i.ctrlKey; this.metaKey = !!i.metaKey; this.shiftKey = !!i.shiftKey;
    }
    getModifierState(k) { return { Alt: this.altKey, Control: this.ctrlKey, Meta: this.metaKey, Shift: this.shiftKey }[k] || false; }
  }
  class PointerEvent extends MouseEvent {
    constructor(t, i = {}) { super(t, i); this.pointerId = i.pointerId ?? 1; this.pointerType = i.pointerType || 'mouse'; this.isPrimary = i.isPrimary ?? true; this.width = this.height = 1; this.pressure = i.pressure ?? 0; }
  }
  class KeyboardEvent extends UIEvent {
    constructor(t, i = {}) { super(t, i); this.key = i.key || ''; this.code = i.code || ''; this.altKey = !!i.altKey; this.ctrlKey = !!i.ctrlKey; this.metaKey = !!i.metaKey; this.shiftKey = !!i.shiftKey; this.repeat = false; }
  }
  class InputEvent extends UIEvent { constructor(t, i = {}) { super(t, i); this.data = i.data ?? null; this.inputType = i.inputType || 'insertText'; } }
  class FocusEvent extends UIEvent {}
  const eventClass = (name, Base, fields) => { G[name] = { [name]: class extends Base { constructor(t, i = {}) { super(t, i); for (const [k, d] of Object.entries(fields)) this[k] = i[k] ?? (typeof d === 'function' ? d() : d); } } }[name]; };
  class SubmitEvent extends Event { constructor(t, i = {}) { super(t, i); this.submitter = i.submitter ?? null; } }

  class EventTarget {
    constructor() { Object.defineProperty(this, '__l', { value: {}, writable: true }); }
    // objects made without the constructor (Object.create(EventTarget.prototype)) get theirs on first use
    get __l() { const v = {}; Object.defineProperty(this, '__l', { value: v, writable: true }); return v; }
    addEventListener(type, fn, opts) {
      if (!fn) return;
      const signal = opts && typeof opts === 'object' ? opts.signal : null;
      if (signal != null && !(signal instanceof AbortSignal)) throw new TypeError('Invalid AbortSignal');
      if (signal && signal.aborted) return;
      const once = typeof opts === 'object' && opts && opts.once;
      const list = (this.__l[type] ||= []);
      if (list.some(l => l.fn === fn)) return;
      const entry = { fn, once };
      if (signal) {
        const abort = () => this.removeEventListener(type, fn);
        signal.addEventListener('abort', abort, { once: true });
        entry.cleanup = () => signal.removeEventListener('abort', abort);
      }
      list.push(entry);
    }
    removeEventListener(type, fn) {
      const list = this.__l[type];
      if (!list) return;
      const i = list.findIndex(l => l.fn === fn);
      if (i >= 0) { const removed = list.splice(i, 1)[0]; if (removed.cleanup) removed.cleanup(); }
    }
    __fire(e) {
      e.currentTarget = this;
      const handler = this['on' + e.type];
      const list = (this.__l[e.type] || []).slice();
      if (typeof handler === 'function') list.unshift({ fn: handler });
      for (const l of list) {
        try {
          if (typeof l.fn === 'function') l.fn.call(this, e); else if (l.fn && l.fn.handleEvent) l.fn.handleEvent(e);
        } catch (err) { log('listener error:', String(err), String(err && err.stack || '')); }
        if (l.once) this.removeEventListener(e.type, l.fn);
        if (e.__stopNow) break;
      }
    }
    dispatchEvent(e) {
      if (e.__dispatching) throw new Error('Event is already being dispatched');
      e.__dispatching = true;
      e.target = this;
      // target, its ancestors, the document, the window
      const path = [];
      let n = this;
      while (n) {
        path.push(n);
        if (n === G) break;
        n = n === document ? G : wrap(upId(n.__sepiaNode));
      }
      e.__path = path;
      try {
        for (const t of path) {
          t.__fire(e);
          if (!e.bubbles || e.__stop) break;
        }
        return !e.defaultPrevented;
      } finally {
        e.__dispatching = false;
        e.currentTarget = null;
      }
    }
  }

  // ----- nodes ----------------------------------------------------------
  class NodeBase extends EventTarget {
    constructor(id) {
      super();
      if (id === undefined) {
        // `new MyElement()` / `super()` of a custom element
        if (upgrading !== undefined) {
          id = upgrading; upgrading = undefined;
          // an upgrade happens in place: whoever holds the element keeps it
          const old = wrappers.get(id);
          if (old && old !== this) { Object.setPrototypeOf(old, new.target.prototype); return old; }
        }
        else { const name = names.get(new.target); if (!name) throw new TypeError('Illegal constructor'); id = __dom_create(name); }
        wrappers.set(id, this);
      }
      Object.defineProperty(this, '__sepiaNode', { value: id });
      // one object per node, whoever constructs it
      if (typeof id === 'number' && !wrappers.has(id)) wrappers.set(id, this);
    }
    get nodeType() { return __dom_kind(this.__sepiaNode); }
    get nodeName() { const t = __dom_tag(this.__sepiaNode); return t.startsWith('#') ? t : t.toUpperCase(); }
    get parentNode() { return wrap(__dom_parent(this.__sepiaNode)); }
    get parentElement() { const p = parentId(this.__sepiaNode); return p >= 0 && __dom_kind(p) === 1 ? wrap(p) : null; }
    get childNodes() { return __dom_children(this.__sepiaNode).map(wrap); }
    get firstChild() { return wrap(__dom_children(this.__sepiaNode)[0] ?? -1); }
    get lastChild() { const c = __dom_children(this.__sepiaNode); return wrap(c[c.length - 1] ?? -1); }
    get nextSibling() { return siblingOf(this, 1, false); }
    get previousSibling() { return siblingOf(this, -1, false); }
    get isConnected() { return isConnectedId(this.__sepiaNode); }
    get ownerDocument() { return document; }
    get nodeValue() { return null; }
    set nodeValue(_) {}
    get textContent() { return __dom_text(this.__sepiaNode); }
    set textContent(v) { __dom_set_text(this.__sepiaNode, v == null ? '' : String(v)); }
    appendChild(c) { __dom_insert(this.__sepiaNode, toNode(c), -1); return c; }
    insertBefore(c, ref) { __dom_insert(this.__sepiaNode, toNode(c), idOf(ref)); return c; }
    removeChild(c) { __dom_remove(c.__sepiaNode); return c; }
    replaceChild(n, old) { __dom_insert(this.__sepiaNode, toNode(n), old.__sepiaNode); __dom_remove(old.__sepiaNode); return old; }
    append(...cs) { for (const c of cs) __dom_insert(this.__sepiaNode, toNode(c), -1); }
    prepend(...cs) { const first = __dom_children(this.__sepiaNode)[0] ?? -1; for (const c of cs) __dom_insert(this.__sepiaNode, toNode(c), first); }
    replaceChildren(...cs) { __dom_clear(this.__sepiaNode); for (const c of cs) __dom_insert(this.__sepiaNode, toNode(c), -1); }
    remove() { __dom_remove(this.__sepiaNode); }
    before(...cs) { const p = parentId(this.__sepiaNode); if (p >= 0) for (const c of cs) __dom_insert(p, toNode(c), this.__sepiaNode); }
    after(...cs) { const p = parentId(this.__sepiaNode); if (p < 0) return; const next = idOf(siblingOf(this, 1, false)); for (const c of cs) __dom_insert(p, toNode(c), next); }
    replaceWith(...cs) { NodeBase.prototype.before.apply(this, cs); __dom_remove(this.__sepiaNode); }
    contains(o) { return isNode(o) && containsId(this.__sepiaNode, o.__sepiaNode); }
    hasChildNodes() { return __dom_children(this.__sepiaNode).length > 0; }
    cloneNode(deep) {
      if (this.nodeType === 11) { const f = document.createDocumentFragment(); if (deep) for (const c of kidsOf(this)) __dom_insert(f.__sepiaNode, NodeBase.prototype.cloneNode.call(c, true).__sepiaNode, -1); return f; }
      const html = __dom_html(this.__sepiaNode, true); const tmp = __dom_create('div'); __dom_set_html(tmp, html); const c = __dom_children(tmp)[0]; if (!deep && c !== undefined) __dom_clear(c); return wrap(c); }
    isSameNode(o) { return o === this; }
    compareDocumentPosition(other) {
      if (!isNode(other)) throw new TypeError("Failed to execute 'compareDocumentPosition': parameter 1 is not of type 'Node'.");
      if (other === this) return 0;
      const chain = id => { const out = []; for (let n = id; n >= 0; n = parentId(n)) out.unshift(n); return out; };
      const a = chain(this.__sepiaNode), b = chain(other.__sepiaNode);
      if (a[0] !== b[0]) return 1 | 32 | (a[0] < b[0] ? 4 : 2);
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      if (i === a.length) return 16 | 4;   // other is inside this
      if (i === b.length) return 8 | 2;    // other contains this
      const kids = __dom_children(a[i - 1]);
      return kids.indexOf(b[i]) < kids.indexOf(a[i]) ? 2 : 4;
    }
    isEqualNode(o) {
      if (!o || o.nodeType !== this.nodeType) return false;
      if (this.nodeType !== 1) return this.nodeType === 9 || this.nodeType === 11 ? this.childNodes.length === o.childNodes.length && this.childNodes.every((c, i) => c.isEqualNode(o.childNodes[i])) : this.nodeValue === o.nodeValue;
      if (this.tagName !== o.tagName) return false;
      const a = this.getAttributeNames(), b = o.getAttributeNames();
      if (a.length !== b.length || a.some(n => o.getAttribute(n) !== this.getAttribute(n))) return false;
      const x = this.childNodes, y = o.childNodes;
      return x.length === y.length && x.every((c, i) => c.isEqualNode(y[i]));
    }
    getRootNode() { let n = this.__sepiaNode; for (let p = parentId(n); p >= 0; p = parentId(n)) n = p; return wrap(n); }
  }
  class CharacterData extends NodeBase {
    constructor(id) {
      if (new.target === CharacterData) throw new TypeError('Illegal constructor');
      super(id);
    }
    get data() { return this.textContent; }
    set data(v) { this.textContent = v; }
    get nodeValue() { return this.textContent; }
    set nodeValue(v) { this.textContent = v; }
    get length() { return this.textContent.length; }
    __offset(value) {
      const offset = value >>> 0;
      if (offset > this.length) {
        throw new DOMException('Offset exceeds character data length', 'IndexSizeError');
      }
      return offset;
    }
    substringData(offset, count) { offset = this.__offset(offset); return this.data.slice(offset, offset + (count >>> 0)); }
    appendData(data) { this.data += String(data); }
    insertData(offset, data) { this.replaceData(offset, 0, data); }
    deleteData(offset, count) { this.replaceData(offset, count, ''); }
    replaceData(offset, count, data) {
      offset = this.__offset(offset);
      this.data = this.data.slice(0, offset) + String(data) + this.data.slice(offset + (count >>> 0));
    }
    cloneNode() { return this.nodeType === 8 ? new Comment(this.data) : new Text(this.data); }
  }
  class Text extends CharacterData {
    constructor(data = '', handle) { super(handle === nodeHandle ? data : __dom_create_text(String(data))); wrappers.set(this.__sepiaNode, this); }
    splitText(offset) {
      offset = this.__offset(offset);
      const tail = new Text(this.data.slice(offset));
      this.data = this.data.slice(0, offset);
      const p = parentId(this.__sepiaNode); if (p >= 0) __dom_insert(p, tail.__sepiaNode, idOf(siblingOf(this, 1, false)));
      return tail;
    }
  }
  class Comment extends CharacterData {
    constructor(data = '', handle) { super(handle === nodeHandle ? data : __dom_create_comment(String(data))); wrappers.set(this.__sepiaNode, this); }
  }

  class ClassList {
    constructor(el, attr = 'class') { this.el = el; this.attr = attr; }
    get __list() { return (this.el.getAttribute(this.attr) || '').split(/\s+/).filter(Boolean); }
    __set(list) { this.el.setAttribute(this.attr, list.join(' ')); }
    // relList: the link types this browser acts on (or lets pages polyfill nothing for)
    supports(t) { return ['stylesheet', 'modulepreload', 'preload', 'prefetch', 'preconnect', 'dns-prefetch', 'icon', 'alternate', 'canonical', 'manifest', 'noopener', 'noreferrer', 'nofollow'].includes(String(t).toLowerCase()); }
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
    get tagName() { return __dom_tag(this.__sepiaNode).toUpperCase(); }
    get localName() { return __dom_tag(this.__sepiaNode); }
    get id() { return this.getAttribute('id') || ''; }
    set id(v) { this.setAttribute('id', v); }
    get className() { return this.getAttribute('class') || ''; }
    set className(v) { this.setAttribute('class', v); }
    get classList() { return new ClassList(this); }
    get relList() { return new ClassList(this, 'rel'); }
    attachShadow(init = {}) {
      if (__dom_shadow(this.__sepiaNode) >= 0) throw new DOMException('Shadow root cannot be created on a host which already hosts a shadow tree.', 'NotSupportedError');
      const mode = init.mode === 'closed' ? 'closed' : 'open';
      const root = __dom_create('#fragment');
      __dom_set_attr(root, 'mode', mode);
      __dom_attach_shadow(this.__sepiaNode, root);
      const sr = wrap(root);
      if (mode === 'closed') Object.defineProperty(this, '__closedRoot', { value: sr });
      return sr;
    }
    get shadowRoot() { const r = __dom_shadow(this.__sepiaNode); return r >= 0 && __dom_get_attr(r, 'mode') !== 'closed' ? wrap(r) : null; }
    get assignedSlot() {
      const host = parentOf(this);
      const r = host ? __dom_shadow(host.__sepiaNode) : -1;
      if (r < 0) return null;
      const name = this.nodeType === 1 ? (this.getAttribute('slot') || '') : '';
      return wrap(__dom_query(r, name ? `slot[name="${CSS.escape(name)}"]` : 'slot:not([name]), slot[name=""]', false)[0] ?? -1);
    }
    get slot() { return this.getAttribute('slot') || ''; }
    set slot(v) { this.setAttribute('slot', v); }
    get style() { return styleObject(this); }
    set style(v) { this.setAttribute('style', String(v)); }
    get dataset() {
      const el = this;
      return new Proxy({}, {
        get(_, k) { if (typeof k !== 'string') return undefined; const v = el.getAttribute('data-' + camelToKebab(k)); return v === null ? undefined : v; },
        set(_, k, v) { el.setAttribute('data-' + camelToKebab(String(k)), String(v)); return true; },
        deleteProperty(_, k) { el.removeAttribute('data-' + camelToKebab(String(k))); return true; },
        has(_, k) { return el.hasAttribute('data-' + camelToKebab(String(k))); },
        ownKeys() { return __dom_attr_names(el.__sepiaNode).filter(n => n.startsWith('data-') && n !== 'data-sepia-id').map(n => kebabToCamel(n.slice(5))); },
        getOwnPropertyDescriptor(_, k) { const v = el.getAttribute('data-' + camelToKebab(String(k))); return v === null ? undefined : { value: v, enumerable: true, configurable: true }; },
      });
    }
    get attributes() {
      const el = this;
      const list = __dom_attr_names(this.__sepiaNode).map(name => ({ name, localName: name, nodeName: name, prefix: null, namespaceURI: null, nodeType: 2, ownerElement: el, specified: true,
        get value() { return el.getAttribute(name) ?? ''; }, set value(v) { el.setAttribute(name, v); }, get nodeValue() { return this.value; }, get textContent() { return this.value; } }));
      list.item = i => list[i] ?? null;
      list.getNamedItem = n => list.find(a => a.name === String(n).toLowerCase()) ?? null;
      list.getNamedItemNS = (_, n) => list.getNamedItem(n);
      list.removeNamedItem = n => { const a = list.getNamedItem(n); el.removeAttribute(n); return a; };
      list.setNamedItem = a => { el.setAttribute(a.name, a.value); return null; };
      return list;
    }
    hasAttributes() { return __dom_attr_names(this.__sepiaNode).length > 0; }
    hasAttributeNS(_, n) { return this.hasAttribute(n); }
    getAttributeNS(_, n) { return this.getAttribute(n); }
    setAttributeNS(_, n, v) { this.setAttribute(String(n).replace(/^.*:/, ''), v); }
    removeAttributeNS(_, n) { this.removeAttribute(n); }
    getAttributeNode(n) { return this.attributes.getNamedItem(n); }
    getAttribute(n) { return __dom_get_attr(this.__sepiaNode, String(n)); }
    setAttribute(n, v) {
      n = String(n);
      const watch = this.attributeChangedCallback && (this.constructor.observedAttributes || []).includes(n);
      const old = watch ? this.getAttribute(n) : null;
      __dom_set_attr(this.__sepiaNode, n, String(v));
      if (watch) this.attributeChangedCallback(n, old, String(v));
    }
    removeAttribute(n) { __dom_remove_attr(this.__sepiaNode, String(n)); }
    hasAttribute(n) { return __dom_get_attr(this.__sepiaNode, String(n)) !== null; }
    toggleAttribute(n, force) { const has = this.hasAttribute(n); const want = force === undefined ? !has : !!force; if (want) this.setAttribute(n, ''); else this.removeAttribute(n); return want; }
    getAttributeNames() { return __dom_attr_names(this.__sepiaNode); }
    get children() { return nodeList(elementKids(this)); }
    get childElementCount() { return elementKids(this).length; }
    get firstElementChild() { return elementKids(this)[0] || null; }
    get lastElementChild() { const c = elementKids(this); return c[c.length - 1] || null; }
    get nextElementSibling() { return siblingOf(this, 1, true); }
    get previousElementSibling() { return siblingOf(this, -1, true); }
    get innerHTML() { return __dom_html(this.__sepiaNode, false); }
    set innerHTML(v) { __dom_set_html(this.__sepiaNode, String(v)); }
    get outerHTML() { return __dom_html(this.__sepiaNode, true); }
    get innerText() { return this.textContent; }
    set innerText(v) { this.textContent = v; }
    insertAdjacentHTML(pos, html) {
      const tmp = __dom_create('div');
      __dom_set_html(tmp, String(html));
      const nodes = __dom_children(tmp);
      const p = parentId(this.__sepiaNode);
      const first = __dom_children(this.__sepiaNode)[0] ?? -1, next = idOf(siblingOf(this, 1, false));
      for (const n of nodes) {
        switch (String(pos).toLowerCase()) {
          case 'beforebegin': if (p >= 0) __dom_insert(p, n, this.__sepiaNode); break;
          case 'afterbegin': __dom_insert(this.__sepiaNode, n, first); break;
          case 'afterend': if (p >= 0) __dom_insert(p, n, next); break;
          default: __dom_insert(this.__sepiaNode, n, -1);
        }
      }
    }
    insertAdjacentElement(pos, el) { this.insertAdjacentHTML(pos, ''); if (pos === 'beforeend') this.append(el); else if (pos === 'afterbegin') this.prepend(el); else if (pos === 'beforebegin') this.before(el); else this.after(el); return el; }
    insertAdjacentText(pos, text) { this.insertAdjacentElement(pos, document.createTextNode(text)); }
    querySelector(s) { return wrap(__dom_query(this.__sepiaNode, String(s), false)[0] ?? -1); }
    querySelectorAll(s) { return nodeList(__dom_query(this.__sepiaNode, String(s), true).map(wrap)); }
    getElementsByTagName(t) { return nodeList(__dom_query(this.__sepiaNode, String(t), true).map(wrap)); }
    getElementsByClassName(c) { return nodeList(__dom_query(this.__sepiaNode, String(c).split(/\s+/).filter(Boolean).map(x => '.' + x).join(''), true).map(wrap)); }
    matches(s) { return __dom_matches(this.__sepiaNode, String(s)); }
    closest(s) { for (let e = this; e && e.nodeType === 1; e = e.parentElement) if (e.matches(s)) return e; return null; }
    focus() { __dom_focus(this.__sepiaNode); this.dispatchEvent(new FocusEvent('focus')); }
    blur() {}
    click() { this.dispatchEvent(new MouseEvent('click', { bubbles: true })); }
    scrollIntoView() {}
    scrollTo() {}
    // geometry: the box our layout gave the element (page coordinates)
    getBoundingClientRect() { const r = __dom_rect(this.__sepiaNode); return r ? new G.DOMRect(r[0] - (G.scrollX || 0), r[1] - (G.scrollY || 0), r[2], r[3]) : new G.DOMRect(); }
    getClientRects() { const r = __dom_rect(this.__sepiaNode); return r ? [new G.DOMRect(r[0] - (G.scrollX || 0), r[1] - (G.scrollY || 0), r[2], r[3])] : []; }
    get offsetWidth() { const r = __dom_rect(this.__sepiaNode); return r ? Math.round(r[2]) : 0; }
    get offsetHeight() { const r = __dom_rect(this.__sepiaNode); return r ? Math.round(r[3]) : 0; }
    get offsetParent() {
      // the root and the body have none (a walk up offsetParent ends there)
      if (this.localName === 'body' || this.localName === 'html' || !this.isConnected) return null;
      if (!__dom_rect(this.__sepiaNode)) return null;
      for (let p = this.parentElement; p; p = p.parentElement) {
        if (p.localName === 'body' || p.localName === 'html') return document.body;
        const pos = p.getAttribute('style') || '';
        if (/position\s*:\s*(relative|absolute|fixed|sticky)/.test(pos) || ['td', 'th', 'table'].includes(p.localName)) return p;
      }
      return document.body;
    }
    get offsetTop() { const r = __dom_rect(this.__sepiaNode); if (!r) return 0; const p = this.offsetParent; const q = p && p !== document.body ? __dom_rect(p.__sepiaNode) : null; return Math.round(r[1] - (q ? q[1] : 0)); }
    get offsetLeft() { const r = __dom_rect(this.__sepiaNode); if (!r) return 0; const p = this.offsetParent; const q = p && p !== document.body ? __dom_rect(p.__sepiaNode) : null; return Math.round(r[0] - (q ? q[0] : 0)); }
    get clientWidth() { if (this.localName === 'html') return G.innerWidth; const r = __dom_rect(this.__sepiaNode); return r ? Math.round(r[2]) : 0; }
    get clientHeight() { if (this.localName === 'html') return G.innerHeight; const r = __dom_rect(this.__sepiaNode); return r ? Math.round(r[3]) : 0; }
    get clientTop() { return 0; } get clientLeft() { return 0; }
    get scrollWidth() { if (this.localName === 'html' || this.localName === 'body') { const d = __dom_rect(0); return d ? Math.round(Math.max(d[2], G.innerWidth)) : G.innerWidth; } return this.clientWidth; }
    get scrollHeight() { if (this.localName === 'html' || this.localName === 'body') { const d = __dom_rect(0); return d ? Math.round(Math.max(d[3], G.innerHeight)) : G.innerHeight; } return this.clientHeight; }
    get scrollTop() { return this.localName === 'html' || this.localName === 'body' ? (G.scrollY || 0) : 0; } set scrollTop(_) {} get scrollLeft() { return 0; } set scrollLeft(_) {}
    get tabIndex() { const v = this.getAttribute('tabindex'); return v === null ? -1 : Number(v); }
    set tabIndex(v) { this.setAttribute('tabindex', String(v)); }
    get value() { const v = this.getAttribute('value'); if (v !== null) return v; return this.localName === 'textarea' ? this.textContent : this.localName === 'select' ? selectValue(this) : ''; }
    set value(v) { this.setAttribute('value', v == null ? '' : String(v)); }
    get form() { return this.closest('form'); }
    animate() { return { finished: Promise.resolve(), cancel() {}, onfinish: null }; }
  }
  for (const name of reflected) {
    Object.defineProperty(Element.prototype, name, {
      get() {
        const v = this.getAttribute(name);
        // URL attributes read back absolute, like a browser's
        if ((name === 'href' || name === 'src' || name === 'action') && v !== null) { try { return new URL(v, document.baseURI).href; } catch (_) { return v; } }
        if (name === 'action' && v === null) return location.href;
        return v ?? '';
      },
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
  // the HTML element interfaces: name → tags (the DOM's spelling, for instanceof)
  const interfaces = {
    Div: 'div', Span: 'span', Anchor: 'a', Button: 'button', Input: 'input', TextArea: 'textarea', Select: 'select',
    Option: 'option', OptGroup: 'optgroup', DataList: 'datalist', Form: 'form', FieldSet: 'fieldset', Legend: 'legend',
    Label: 'label', Output: 'output', Progress: 'progress', Meter: 'meter', Image: 'img', Picture: 'picture',
    Source: 'source', Track: 'track', Paragraph: 'p', Heading: 'h1 h2 h3 h4 h5 h6', UList: 'ul', OList: 'ol',
    LI: 'li', DList: 'dl', Menu: 'menu', Table: 'table', TableSection: 'thead tbody tfoot', TableRow: 'tr',
    TableCell: 'td th', TableCaption: 'caption', TableCol: 'col colgroup', Template: 'template', Canvas: 'canvas',
    Script: 'script', Style: 'style', Link: 'link', Meta: 'meta', Base: 'base', Title: 'title', Body: 'body',
    Html: 'html', Head: 'head', Pre: 'pre', Quote: 'blockquote q', Mod: 'ins del', HR: 'hr', BR: 'br',
    Time: 'time', Data: 'data', IFrame: 'iframe', Embed: 'embed', Object: 'object', Map: 'map', Area: 'area',
    Dialog: 'dialog', Details: 'details', Slot: 'slot',
  };
  const tagClass = {};
  classes.Media = class HTMLMediaElement extends HTMLElement {};
  for (const [n, tags] of Object.entries(interfaces)) {
    classes[n] = { [`HTML${n}Element`]: class extends HTMLElement {} }[`HTML${n}Element`];
    for (const t of tags.split(' ')) tagClass[t] = n;
  }
  for (const [n, t] of [['Video', 'video'], ['Audio', 'audio']]) {
    classes[n] = { [`HTML${n}Element`]: class extends classes.Media {} }[`HTML${n}Element`];
    tagClass[t] = n;
  }
  classes.Unknown = class HTMLUnknownElement extends HTMLElement {};
  const SVGElement = class SVGElement extends Element {};
  const SVGGraphicsElement = class SVGGraphicsElement extends SVGElement {};
  const SVGSVGElement = class SVGSVGElement extends SVGGraphicsElement {};
  const svgTags = new Set(['svg', 'path', 'g', 'circle', 'rect', 'line', 'polyline', 'polygon', 'ellipse', 'text', 'tspan', 'use', 'defs', 'symbol', 'clippath', 'mask', 'lineargradient', 'radialgradient', 'stop', 'image', 'foreignobject', 'title', 'desc']);
  const inSvg = id => { for (let p = __dom_parent(id); p >= 0 && p !== undefined; p = __dom_parent(p)) if (__dom_tag(p) === 'svg') return true; return false; };
  function makeElement(id) {
    const tag = __dom_tag(id);
    const custom = defined.get(tag);
    if (custom) {
      upgrading = id;
      try { return new custom(); } catch (e) { log('custom element', tag, String(e && e.message), String(e && e.stack)); } finally { upgrading = undefined; }
    }
    if (tag === 'svg') return new SVGSVGElement(id);
    if (svgTags.has(tag) && tag !== 'title' && tag !== 'image' && inSvg(id)) return new (tag === 'defs' || tag === 'stop' ? SVGElement : SVGGraphicsElement)(id);
    const C = classes[tagClass[tag]] || HTMLElement;
    return new C(id);
  }
  // connectedCallback / disconnectedCallback around every DOM change
  function connectAll(id, on) {
    if (defined.size === 0) return;
    const root = wrap(id);
    if (!root || root.nodeType !== 1 || root.isConnected !== on) return;
    const found = [root, ...root.querySelectorAll([...defined.keys()].join(','))];
    for (const el of found) {
      if (!defined.has(__dom_tag(el.__sepiaNode)) || !!el.__connected === on) continue;
      el.__connected = on;
      const cb = on ? el.connectedCallback : el.disconnectedCallback;
      if (typeof cb === 'function') { try { cb.call(el); } catch (e) { log('custom element', el.localName, e); } }
    }
  }
  {
    const insert = G.__dom_insert, remove = G.__dom_remove, html = G.__dom_set_html;
    // a fragment hands over its children: they are what gets connected
    G.__dom_insert = (p, c, ref) => {
      const moved = __dom_kind(c) === 11 ? __dom_children(c) : [c];
      const r = insert(p, c, ref);
      for (const m of moved) connectAll(m, true);
      return r;
    };
    G.__dom_remove = id => { const was = defined.size && wrap(id) && wrap(id).isConnected; const r = remove(id); if (was) connectAll(id, false); return r; };
    G.__dom_set_html = (id, v) => { const r = html(id, v); upgradeTree(id); for (const c of __dom_children(id)) connectAll(c, true); return r; };
  }
  // new markup (innerHTML, cloneNode, importNode) builds defined elements at once,
  // in document order, as a browser does; template contents stay inert
  function upgradeTree(id) {
    if (defined.size === 0) return;
    const walk = n => {
      for (const c of __dom_children(n)) {
        if (__dom_kind(c) !== 1) continue;
        const tag = __dom_tag(c);
        if (defined.has(tag) && !wrappers.has(c)) wrap(c);
        if (tag !== 'template') walk(c);
      }
    };
    walk(id);
  }
  // a template's content is its children
  // a template's content is one fragment of its own: the parsed children move
  // there on first use, and markup set later goes there too
  const contentOf = new WeakMap();
  Object.defineProperty(classes.Template.prototype, 'content', {
    get() {
      let f = contentOf.get(this);
      if (!f) {
        f = document.createDocumentFragment();
        for (const c of __dom_children(this.__sepiaNode)) __dom_insert(f.__sepiaNode, c, -1);
        contentOf.set(this, f);
        __dom_set_template_content(this.__sepiaNode, f.__sepiaNode);
      }
      return f;
    },
    configurable: true,
  });
  Object.defineProperty(classes.Template.prototype, 'innerHTML', {
    get() { return __dom_html(this.__sepiaNode, false); },
    set(v) { const f = this.content; f.replaceChildren(); const box = document.createElement('div'); box.innerHTML = String(v); f.append(...box.childNodes); },
    configurable: true,
  });
  // <a>/<area> take their URL apart like `location` does
  for (const C of [classes.Anchor, classes.Area]) {
    const url = el => { try { return new URL(el.getAttribute('href') ?? '', document.baseURI); } catch (_) { return null; } };
    for (const part of ['protocol', 'host', 'hostname', 'port', 'pathname', 'search', 'hash', 'username', 'password']) {
      Object.defineProperty(C.prototype, part, {
        get() { const u = this.hasAttribute('href') ? url(this) : null; return u ? u[part] : ''; },
        set(v) { const u = url(this); if (u) { u[part] = v; this.setAttribute('href', u.href); } },
        configurable: true,
      });
    }
    Object.defineProperty(C.prototype, 'origin', { get() { const u = this.hasAttribute('href') ? url(this) : null; return u ? u.origin : ''; }, configurable: true });
    C.prototype.toString = function () { return this.href; };
  }
  classes.Form.prototype.submit = function () { submitForm(this); };
  classes.Form.prototype.reset = function () {};
  classes.Form.prototype.requestSubmit = function (submitter = null) {
    if (submitter !== null) {
      if (!(submitter instanceof Element) || !isSubmitButton(submitter)) throw new TypeError('Not a submit button');
      if (submitter.form !== this) throw new Error('Submitter does not belong to this form');
    }
    if (this.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true, submitter }))) submitForm(this, submitter);
  };
  // <slot>: the host's children assigned to it
  classes.Slot.prototype.assignedNodes = function (opts = {}) {
    const root = this.getRootNode();
    const host = root instanceof ShadowRoot ? root.host : null;
    if (!host) return opts.flatten ? kidsOf(this) : [];
    const name = this.getAttribute('name') || '';
    const nodes = kidsOf(host).filter(n => name ? n.nodeType === 1 && n.getAttribute('slot') === name : (n.nodeType !== 1 || !n.getAttribute('slot')));
    return nodes.length || !opts.flatten ? nodes : kidsOf(this);
  };
  classes.Slot.prototype.assignedElements = function (opts) { return this.assignedNodes(opts).filter(n => n.nodeType === 1); };
  classes.Slot.prototype.assign = function () {};
  // form.elements: its controls, by index and by name/id
  Object.defineProperty(classes.Form.prototype, 'elements', {
    get() {
      const list = this.querySelectorAll('input, select, textarea, button, fieldset, output, object');
      const named = n => list.find(e => e.getAttribute('name') === n || e.id === n) ?? null;
      for (const e of list) for (const key of [e.getAttribute('name'), e.id]) if (key && !(key in list) && isNaN(key)) list[key] = e;
      list.namedItem = named;
      return list;
    },
    configurable: true,
  });
  Object.defineProperty(classes.Form.prototype, 'length', { get() { return this.elements.length; }, configurable: true });
  classes.Dialog.prototype.showModal = function () { this.setAttribute('open', ''); };
  classes.Dialog.prototype.show = function () { this.setAttribute('open', ''); };
  classes.Dialog.prototype.close = function () { this.removeAttribute('open'); this.dispatchEvent(new Event('close')); };
  // no canvas: it would be a fingerprinting surface
  classes.Canvas.prototype.getContext = function () { return null; };
  classes.Canvas.prototype.toDataURL = function () { return 'data:,'; };

  // the Node interface's constants, on the interface and on every node
  {
    const consts = {
      ELEMENT_NODE: 1, ATTRIBUTE_NODE: 2, TEXT_NODE: 3, CDATA_SECTION_NODE: 4, ENTITY_REFERENCE_NODE: 5, ENTITY_NODE: 6,
      PROCESSING_INSTRUCTION_NODE: 7, COMMENT_NODE: 8, DOCUMENT_NODE: 9, DOCUMENT_TYPE_NODE: 10, DOCUMENT_FRAGMENT_NODE: 11, NOTATION_NODE: 12,
      DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2, DOCUMENT_POSITION_FOLLOWING: 4,
      DOCUMENT_POSITION_CONTAINS: 8, DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32,
    };
    for (const [k, v] of Object.entries(consts)) {
      Object.defineProperty(NodeBase, k, { value: v, enumerable: true });
      Object.defineProperty(NodeBase.prototype, k, { value: v, enumerable: true });
    }
  }
  class DocumentFragment extends NodeBase {
    querySelector(s) { return Element.prototype.querySelector.call(this, s); }
    querySelectorAll(s) { return Element.prototype.querySelectorAll.call(this, s); }
    get children() { return nodeList(elementKids(this)); }
  }

  class ShadowRoot extends DocumentFragment {
    get host() { return wrap(__dom_shadow(this.__sepiaNode)); }
    get mode() { return __dom_get_attr(this.__sepiaNode, 'mode') || 'open'; }
    get delegatesFocus() { return false; }
    get slotAssignment() { return 'named'; }
    get innerHTML() { return __dom_html(this.__sepiaNode, false); }
    set innerHTML(v) { __dom_set_html(this.__sepiaNode, String(v)); }
    get activeElement() { return null; }
    get styleSheets() { return this.querySelectorAll('style').map(e => e.sheet).filter(Boolean); }
    get adoptedStyleSheets() { return this.__adopted || []; }
    set adoptedStyleSheets(list) { Object.defineProperty(this, '__adopted', { value: [...list], writable: true, configurable: true }); __css_set(this.__sepiaNode, this.__adopted.map(s => s.__rules.join('\n')).join('\n')); }
    getElementById(id) { return this.querySelector('#' + CSS.escape(String(id))); }
    elementFromPoint() { return null; }
    getSelection() { return G.getSelection(); }
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
    get domain() { return location.hostname; }
    set domain(v) {}
    get baseURI() { const b = this.querySelector('base[href]'); try { return b ? new URL(b.getAttribute('href'), location.href).href : location.href; } catch (e) { return location.href; } }
    get contentType() { return 'text/html'; }
    get lastModified() { return '01/01/1970 00:00:00'; }
    get forms() { return this.querySelectorAll('form'); }
    get images() { return this.querySelectorAll('img'); }
    get links() { return this.querySelectorAll('a[href], area[href]'); }
    get scripts() { return this.querySelectorAll('script'); }
    get styleSheets() { return []; }
    get doctype() { return { name: 'html', publicId: '', systemId: '' }; }
    get designMode() { return 'off'; }
    get dir() { return this.documentElement ? (this.documentElement.getAttribute('dir') || '') : ''; }
    get readyState() { return G.__readyState || 'loading'; }
    get visibilityState() { return 'visible'; }
    get hidden() { return false; }
    get URL() { return location.href; }
    get location() { return location; }
    get defaultView() { return G; }
    get activeElement() { return this.body; }
    get currentScript() { return G.__currentScript || null; }
    get characterSet() { return 'UTF-8'; }
    get compatMode() { return 'CSS1Compat'; }
    get scrollingElement() { return this.documentElement; }
    get fonts() { return { ready: Promise.resolve(), load: () => Promise.resolve([]), check: () => true, add() {}, forEach() {} }; }
    createElement(t) { return wrap(__dom_create(String(t))); }
    createElementNS(_, t) { return this.createElement(t); }
    createTextNode(t) { return wrap(__dom_create_text(String(t))); }
    createComment(data = '') { return new Comment(data); }
    createDocumentFragment() { return wrap(__dom_create('#fragment')); }
    createEvent(type) {
      switch (String(type).toLowerCase()) {
        case 'event': case 'events': case 'htmlevents': return new Event('', { cancelable: false });
        case 'customevent': return new CustomEvent('', { cancelable: false });
        default: throw new TypeError('Unsupported event interface: ' + type);
      }
    }
    getElementById(id) { return wrap(__dom_query(0, '#' + CSS.escape(String(id)), false)[0] ?? -1); }
    getElementsByName(n) { return this.querySelectorAll(`[name="${n}"]`); }
    querySelector(s) { return wrap(__dom_query(0, String(s), false)[0] ?? -1); }
    querySelectorAll(s) { return nodeList(__dom_query(0, String(s), true).map(wrap)); }
    getElementsByTagName(t) { return this.querySelectorAll(t); }
    getElementsByClassName(c) { return Element.prototype.getElementsByClassName.call({ __sepiaNode: 0 }, c); }
    hasFocus() { return true; }
    importNode(n, deep) { return n.cloneNode(!!deep); }
    adoptNode(n) { if (n.parentNode) n.remove(); return n; }
    // during parsing: after the running script; scripts it writes do not run
    write(...parts) {
      const html = parts.join('');
      const at = G.__currentScript && G.__currentScript.parentNode ? G.__currentScript : null;
      const parent = at ? at.parentNode : this.body;
      if (!parent) return;
      const before = new Set(__dom_children(parent.__sepiaNode));
      if (at) at.insertAdjacentHTML('afterend', html); else parent.insertAdjacentHTML('beforeend', html);
      for (const id of __dom_children(parent.__sepiaNode)) if (!before.has(id)) G.__sepia_run_written(id);
    }
    writeln(...parts) { this.write(...parts, '\n'); }
    open() { return this; }
    close() {}
    execCommand() { return false; }
  }

  // ----- the window ------------------------------------------------------
  const document = Object.create(Document.prototype);
  Object.defineProperty(document, '__sepiaNode', { value: 0 });
  Object.defineProperty(document, '__l', { value: {}, writable: true });
  wrappers.set(0, document);

  for (const k of ['addEventListener', 'removeEventListener', '__fire', 'dispatchEvent']) { const f = EventTarget.prototype[k]; G[k] = function (...a) { return f.apply(this == null ? G : this, a); }; }
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

  // Timers use real deadlines. Positive delays are rounded to 100 ms;
  // zero-delay tasks can drain during a bounded settle pass.
  let timerId = 1;
  const timers = new Map();
  const timerDelay = ms => { const n = Number(ms); return Number.isFinite(n) ? Math.max(0, n) : 0; };
  const timerDeadline = ms => ms <= 0 ? Date.now() : Math.ceil((Date.now() + ms) / 100) * 100;
  G.setTimeout = (fn, ms = 0, ...args) => {
    const id = timerId++, delay = timerDelay(ms);
    timers.set(id, { fn, args, delay, deadline: timerDeadline(delay), repeat: false });
    return id;
  };
  G.setInterval = (fn, ms = 0, ...args) => {
    const id = timerId++, delay = Math.max(100, timerDelay(ms));
    timers.set(id, { fn, args, delay, deadline: timerDeadline(delay), repeat: true });
    return id;
  };
  G.clearTimeout = G.clearInterval = id => { timers.delete(id); };
  G.requestAnimationFrame = fn => G.setTimeout(() => fn(G.performance.now()), 16);
  G.cancelAnimationFrame = G.clearTimeout;
  G.requestIdleCallback = fn => G.setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1);
  G.cancelIdleCallback = G.clearTimeout;
  // the engine's own promise jobs: pages may replace the global Promise
  const nativeResolved = Promise.resolve();
  const nativeThen = Promise.prototype.then;
  let microtasks = 0;
  G.queueMicrotask = fn => {
    if (typeof fn !== 'function') throw new TypeError("Failed to execute 'queueMicrotask' on 'Window': parameter 1 is not of type 'Function'.");
    nativeThen.call(nativeResolved, () => fn());
  };
  G.__sepia_tick = () => {
    microtasks = 0;
    let delivered = 0;
    const netBegan = Date.now();
    try { delivered = G.__sepia_net(); } catch (e) { log('network delivery', e); }
    if (Date.now() - netBegan > 500) log('slow network delivery:', Date.now() - netBegan, 'ms,', delivered, 'answers');
    const now = Date.now();
    const due = [...timers.entries()].filter(([, t]) => t.deadline <= now)
      .sort((a, b) => a[1].deadline - b[1].deadline || a[0] - b[0]);
    let ran = 0;
    for (const [id, t] of due) {
      if (!timers.has(id)) continue;
      if (t.repeat) t.deadline = timerDeadline(t.delay); else timers.delete(id);
      const began = Date.now();
      try { typeof t.fn === 'function' ? t.fn(...t.args) : (0, eval)(String(t.fn)); } catch (e) { log('timer error:', String(e), String(e && e.stack || '')); }
      if (Date.now() - began > 500) log('slow timer:', Date.now() - began, 'ms', String(t.fn).slice(0, 160));
      ran++;
    }
    return ran + delivered;
  };
  G.__sepia_next_timer = () => {
    let next = Infinity;
    for (const timer of timers.values()) next = Math.min(next, timer.deadline);
    return next === Infinity ? -1 : Math.max(0, next - Date.now());
  };

  // ----- dates in the forms browsers accept beyond ISO 8601 -----------------
  {
    const NativeDate = Date;
    const nativeParse = Date.parse;
    const months = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'];
    const zone = z => {
      if (!z || /^(z|gmt|utc|ut)$/i.test(z)) return z ? 0 : null;
      const m = /^([+-])(\d{2}):?(\d{2})$/.exec(z);
      return m ? (m[1] === '-' ? -1 : 1) * (Number(m[2]) * 60 + Number(m[3])) : undefined;
    };
    const build = (y, mo, d, h = 0, mi = 0, sec = 0, ms = 0, z = null) => {
      if (z === undefined) return NaN;
      if (z === null) return new NativeDate(y, mo, d, h, mi, sec, ms).getTime();
      return NativeDate.UTC(y, mo, d, h, mi, sec, ms) - z * 60000;
    };
    const fallback = text => {
      const s = String(text).trim();
      let m = /^(\d{4})[-\/](\d{1,2})[-\/](\d{1,2})(?:[T\s]+(\d{1,2}):(\d{2})(?::(\d{2})(?:[.,](\d+))?)?)?\s*(Z|[+-]\d{2}:?\d{2}|GMT|UTC)?$/i.exec(s);
      if (m) {
        const ms = m[7] ? Math.floor(Number((m[7] + '00').slice(0, 3))) : 0;
        // a date alone is UTC, a date and time without a zone is local
        const z = m[8] !== undefined ? zone(m[8]) : (m[4] === undefined && s.includes('-') ? 0 : null);
        return build(+m[1], +m[2] - 1, +m[3], +(m[4] || 0), +(m[5] || 0), +(m[6] || 0), ms, z);
      }
      const t = s.replace(/^[a-z]+,?\s+/i, w => months.includes(w.slice(0, 3).toLowerCase()) ? w : '');
      m = /^([a-z]+)\.?\s+(\d{1,2})(?:st|nd|rd|th)?,?\s+(\d{4})(?:,?\s+(?:at\s+)?(\d{1,2}):(\d{2})(?::(\d{2}))?\s*(am|pm)?)?\s*(Z|[+-]\d{2}:?\d{2}|GMT|UTC)?$/i.exec(t)
        || null;
      let d, mo, y, h, mi, sec, ap, z;
      if (m) [, mo, d, y, h, mi, sec, ap, z] = m;
      else {
        m = /^(\d{1,2})\s+([a-z]+)\.?,?\s+(\d{4})(?:\s+(\d{1,2}):(\d{2})(?::(\d{2}))?\s*(am|pm)?)?\s*(Z|[+-]\d{2}:?\d{2}|GMT|UTC)?$/i.exec(t);
        if (!m) return NaN;
        [, d, mo, y, h, mi, sec, ap, z] = m;
      }
      const month = months.indexOf(mo.slice(0, 3).toLowerCase());
      if (month < 0) return NaN;
      let hour = +(h || 0);
      if (ap) hour = hour % 12 + (ap.toLowerCase() === 'pm' ? 12 : 0);
      return build(+y, month, +d, hour, +(mi || 0), +(sec || 0), 0, z ? zone(z) : null);
    };
    const parse = text => { const n = nativeParse(text); return isNaN(n) ? fallback(text) : n; };
    const D = new Proxy(NativeDate, {
      construct(target, args, newTarget) {
        if (args.length === 1 && typeof args[0] === 'string') {
          const n = nativeParse(args[0]);
          if (isNaN(n)) args = [fallback(args[0])];
        }
        return Reflect.construct(target, args, newTarget === D ? target : newTarget);
      },
    });
    NativeDate.parse = parse;
    G.Date = D;
  }

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
    toString() {
      const encode = value => encodeURIComponent(value).replace(/%20/g, '+').replace(/[!'()~]/g, c => '%' + c.charCodeAt(0).toString(16).toUpperCase());
      return this.__p.map(([k, v]) => `${encode(k)}=${encode(v)}`).join('&');
    }
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
    constructor(init) {
      this.__h = new Map();
      if (!init) return;
      const pairs = init instanceof Headers ? [...init.__h] : Array.isArray(init) || typeof init[Symbol.iterator] === 'function' ? [...init] : Object.entries(init);
      for (const [k, v] of pairs) this.append(k, v);
    }
    get(k) { return this.__h.get(String(k).toLowerCase()) ?? null; }
    set(k, v) { this.__h.set(String(k).toLowerCase(), String(v)); }
    append(k, v) { const key = String(k).toLowerCase(); this.__h.set(key, this.__h.has(key) ? this.__h.get(key) + ', ' + String(v) : String(v)); }
    delete(k) { this.__h.delete(String(k).toLowerCase()); }
    has(k) { return this.__h.has(String(k).toLowerCase()); }
    forEach(f, self) { this.__h.forEach((v, k) => f.call(self, v, k, this)); }
    entries() { return this.__h.entries(); }
    keys() { return this.__h.keys(); }
    values() { return this.__h.values(); }
    [Symbol.iterator]() { return this.__h.entries(); }
  }
  class Response {
    constructor(body, init = {}) { this.__body = body ?? ''; this.status = init.status ?? 200; this.ok = this.status >= 200 && this.status < 300; this.url = init.url || ''; this.headers = new Headers(); this.statusText = ''; this.redirected = false; this.type = 'basic'; }
    text() { return Promise.resolve(this.__body); }
    json() { return Promise.resolve().then(() => JSON.parse(this.__body)); }
    clone() { return new Response(this.__body, { status: this.status, url: this.url }); }
  }
  // the page's own site only, through the page's chain (enforced natively);
  // custom request headers go along, minus the ones a browser forbids
  function bodyOf(b, headers) {
    let type = headers && headers.get('content-type');
    if (b == null) return [undefined, undefined];
    if (b instanceof URLSearchParams) return [b.toString(), type || 'application/x-www-form-urlencoded;charset=UTF-8'];
    if (typeof FormData !== 'undefined' && b instanceof FormData) return [new URLSearchParams(b.__pairs).toString(), 'application/x-www-form-urlencoded;charset=UTF-8'];
    if (typeof b === 'object' && !(typeof b === 'string')) return [JSON.stringify(b), type || 'application/json'];
    return [String(b), type || 'text/plain;charset=UTF-8'];
  }
  function sendNow(url, method, body, headers) {
    const list = headers ? [...new Headers(headers)].filter(([k]) => k !== 'content-type') : [];
    headers = headers ? new Headers(headers) : null;
    const [data, type] = bodyOf(body, headers);
    // the browser filters these natively (see net::script_header)
    return __fetch(String(url), method ? String(method) : (data === undefined ? 'GET' : 'POST'), data, type, list);
  }
  // answers on their way: id -> callback(row)
  const awaiting = new Map();
  G.__sepia_await = (id, cb) => { awaiting.set(id, cb); };
  G.__sepia_net = () => {
    if (!awaiting.size) return 0;
    const rows = __fetch_poll();
    for (const row of rows) { const cb = awaiting.get(row[0]); awaiting.delete(row[0]); if (cb) cb(row); }
    return rows.length;
  };
  // start a request without blocking; done(row) gets [id, status, text, url, headers] or [id, null, error]
  function sendLater(url, method, body, headers, done, noCors) {
    const list = headers ? [...new Headers(headers)].filter(([k]) => k !== 'content-type') : [];
    const h = headers ? new Headers(headers) : null;
    const [data, type] = bodyOf(body, h);
    const id = __fetch_start(String(url), method ? String(method) : (data === undefined ? 'GET' : 'POST'), data, type, list, !!noCors);
    awaiting.set(id, done);
    return id;
  }
  const headersFrom = text => { const h = new Headers(); for (const line of String(text || '').split('\r\n')) { const i = line.indexOf(':'); if (i > 0) h.append(line.slice(0, i).trim(), line.slice(i + 1).trim()); } return h; };
  G.fetch = (input, opts = {}) => new Promise((resolve, reject) => {
    let signal;
    try {
      const url = typeof input === 'string' ? input : input && (input.href || input.url);
      const req = input && typeof input === 'object' && input.url ? input : {};
      signal = opts.signal ?? req.signal;
      if (signal != null) {
        if (!(signal instanceof AbortSignal)) throw new TypeError('Invalid AbortSignal');
        signal.throwIfAborted();
      }
      let settled = false;
      const id = sendLater(url, opts.method || req.method, opts.body ?? req.body, opts.headers || req.headers, row => {
        if (settled) return;
        settled = true;
        if (row[1] === null) { reject(new TypeError(String(row[2] || 'Failed to fetch'))); return; }
        const response = new Response(row[2], { status: row[1], url: row[1] === 0 ? '' : row[3] });
        response.headers = headersFrom(row[4]);
        if (row[1] === 0) response.type = 'opaque';
        resolve(response);
      }, (opts.mode || req.mode) === 'no-cors');
      if (signal) signal.addEventListener('abort', () => { if (settled) return; settled = true; awaiting.delete(id); reject(signal.reason); }, { once: true });
    } catch (e) { reject(signal instanceof AbortSignal && signal.aborted ? signal.reason : e instanceof Error ? e : new TypeError(String(e))); }
  });
  G.Request = class Request {
    constructor(input, o = {}) {
      const from = input instanceof Request ? input : null;
      let url = from ? from.url : String(input && input.href || input);
      try { url = new URL(url, location.href).href; } catch (e) { throw new TypeError(`Failed to parse URL from ${url}`); }
      Object.defineProperty(this, '__r', { value: {
        url, method: String(o.method || (from && from.method) || 'GET').toUpperCase(),
        headers: new Headers(o.headers || (from && from.headers) || undefined),
        body: o.body ?? (from ? from.__r.body : null),
        signal: o.signal ?? (from && from.signal) ?? new G.AbortController().signal,
        mode: o.mode || 'cors', credentials: o.credentials || 'same-origin', cache: o.cache || 'default',
        redirect: o.redirect || 'follow', referrer: o.referrer ?? 'about:client', referrerPolicy: o.referrerPolicy || '',
        integrity: o.integrity || '', keepalive: !!o.keepalive, destination: '',
      } });
    }
    get url() { return this.__r.url; }
    get method() { return this.__r.method; }
    get headers() { return this.__r.headers; }
    get body() { return this.__r.body; }
    get bodyUsed() { return false; }
    get signal() { return this.__r.signal; }
    get mode() { return this.__r.mode; }
    get credentials() { return this.__r.credentials; }
    get cache() { return this.__r.cache; }
    get redirect() { return this.__r.redirect; }
    get referrer() { return this.__r.referrer; }
    get referrerPolicy() { return this.__r.referrerPolicy; }
    get integrity() { return this.__r.integrity; }
    get keepalive() { return this.__r.keepalive; }
    get destination() { return this.__r.destination; }
    clone() { return new Request(this); }
    text() { return Promise.resolve(this.__r.body == null ? '' : String(this.__r.body)); }
    json() { return this.text().then(JSON.parse); }
    arrayBuffer() { return this.text().then(t => new G.TextEncoder().encode(t).buffer); }
  };
  G.FormData = class FormData {
    constructor(form) {
      this.__pairs = form ? formPairs(form) : [];
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
    constructor() { super(); this.readyState = 0; this.status = 0; this.statusText = ''; this.responseText = ''; this.response = ''; this.responseType = ''; this.responseURL = ''; this.__h = {}; this.__rh = new Headers(); this.timeout = 0; this.withCredentials = false; this.upload = new EventTarget(); }
    open(method, url, async = true) { this.__m = method; this.__u = url; this.__async = async !== false; this.readyState = 1; this.__aborted = false; }
    setRequestHeader(k, v) { this.__h[k] = v; }
    getResponseHeader(k) { return this.__rh.get(k); }
    getAllResponseHeaders() { let out = ''; this.__rh.forEach((v, k) => { out += k + ': ' + v + '\r\n'; }); return out; }
    overrideMimeType() {}
    abort() { this.__aborted = true; if (this.__sepiaNode) awaiting.delete(this.__sepiaNode); this.readyState = 0; }
    __fire(type) { if (typeof type === 'string') { this.dispatchEvent(new Event(type)); return; } return super.__fire(type); }
    __done(status, text, finalUrl, headers) {
      Object.assign(this, { status, responseText: text, responseURL: finalUrl, __rh: headers, readyState: 4, statusText: status === 200 ? 'OK' : '' });
      this.response = this.responseType === 'json' ? (() => { try { return JSON.parse(text); } catch (_) { return null; } })()
        : this.responseType === 'arraybuffer' ? new G.TextEncoder().encode(text).buffer
        : this.responseType === 'blob' ? new G.Blob([text], { type: headers.get('content-type') || '' })
        : this.responseType === 'document' ? new G.DOMParser().parseFromString(text, 'text/html') : text;
      this.__fire('readystatechange'); this.__fire('load'); this.__fire('loadend');
    }
    __failed() { this.readyState = 4; this.status = 0; this.__fire('readystatechange'); this.__fire('error'); this.__fire('loadend'); }
    send(body) {
      if (!this.__async) {
        // a synchronous XHR blocks, as it does in a browser
        try { const [status, text, finalUrl, type] = sendNow(this.__u, this.__m, body, this.__h); const h = new Headers(); if (type) h.set('content-type', type); this.__done(status, text, finalUrl, h); } catch (e) { this.__failed(); }
        return;
      }
      this.__fire('loadstart');
      try {
        this.__sepiaNode = sendLater(this.__u, this.__m, body, this.__h, row => {
          if (this.__aborted) return;
          if (row[1] === null) this.__failed(); else this.__done(row[1], row[2], row[3], headersFrom(row[4]));
        });
      } catch (e) { setTimeout(() => { if (!this.__aborted) this.__failed(); }, 0); }
    }
  };
  for (const [k, v] of Object.entries({ UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 })) G.XMLHttpRequest[k] = v;
  const abortStates = new WeakMap();
  function abortSignal(signal, reason) {
    const state = abortStates.get(signal);
    if (state.aborted) return;
    state.aborted = true;
    state.reason = reason === undefined ? new DOMException('The operation was aborted', 'AbortError') : reason;
    signal.dispatchEvent(new Event('abort', { cancelable: false }));
  }
  class AbortSignal extends EventTarget {
    constructor(handle) {
      super();
      if (handle !== nodeHandle) throw new TypeError('Illegal constructor');
      abortStates.set(this, { aborted: false, reason: undefined });
    }
    get aborted() { return abortStates.get(this).aborted; }
    get reason() { return abortStates.get(this).reason; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static abort(reason) { const signal = new AbortSignal(nodeHandle); abortSignal(signal, reason); return signal; }
    static timeout(milliseconds) {
      const delay = Number(milliseconds);
      if (!Number.isFinite(delay) || delay < 0 || delay > Number.MAX_SAFE_INTEGER) throw new RangeError('Invalid timeout');
      const signal = new AbortSignal(nodeHandle);
      G.setTimeout(() => abortSignal(signal, new DOMException('The operation timed out', 'TimeoutError')), delay);
      return signal;
    }
    static any(signals) {
      const sources = Array.from(signals);
      if (sources.some(signal => !(signal instanceof AbortSignal))) throw new TypeError('Invalid AbortSignal');
      const result = new AbortSignal(nodeHandle);
      const ready = sources.find(signal => signal.aborted);
      if (ready) { abortSignal(result, ready.reason); return result; }
      const subscriptions = [];
      for (const signal of sources) {
        const forward = () => {
          abortSignal(result, signal.reason);
          for (const [source, listener] of subscriptions) source.removeEventListener('abort', listener);
        };
        subscriptions.push([signal, forward]);
        signal.addEventListener('abort', forward, { once: true });
      }
      return result;
    }
  }
  G.AbortController = class AbortController {
    constructor() { Object.defineProperty(this, 'signal', { value: new AbortSignal(nodeHandle), enumerable: true }); }
    abort(reason) { abortSignal(this.signal, reason); }
  };
  G.AbortSignal = AbortSignal;

  // the chosen country's locale and time zone (the engine would ask the OS), unless the page names one
  {
    const fixed = persona.language;
    // the zone, if this engine knows it (else its default)
    let zone;
    try { new G.Intl.DateTimeFormat('en-US', { timeZone: persona.timeZone }); zone = persona.timeZone; } catch (e) { zone = undefined; }
    const loc = l => (l === undefined || l === null || (Array.isArray(l) && l.length === 0)) ? fixed : l;
    for (const k of ['Collator', 'DateTimeFormat', 'DisplayNames', 'DurationFormat', 'ListFormat', 'NumberFormat', 'PluralRules', 'RelativeTimeFormat', 'Segmenter']) {
      const C = G.Intl[k];
      if (typeof C !== 'function') continue;
      // dates show the country's time unless the page asks for a zone
      const opt = k === 'DateTimeFormat' ? o => (o && o.timeZone !== undefined ? o : { ...(o || {}), timeZone: zone }) : o => o;
      const P = new Proxy(C, {
        construct: (t, [l, o], nt) => k === 'NumberFormat' ? numberFormat(t, loc(l), o, nt === P ? t : nt) : Reflect.construct(t, [loc(l), opt(o)], nt === P ? t : nt),
        apply: (t, self, [l, o]) => Reflect.apply(t, self, [loc(l), opt(o)]),
      });
      G.Intl[k] = P;
    }
    // the engine's NumberFormat knows plain decimals only: percent, currency
    // and units, non-finite numbers and formatToParts are done here
    function numberFormat(t, l, o, nt) {
      o = o === undefined ? {} : Object(o);
      const style = o.style === undefined ? 'decimal' : String(o.style);
      const inner = { ...o };
      delete inner.style; delete inner.currency; delete inner.currencyDisplay; delete inner.currencySign; delete inner.unit; delete inner.unitDisplay;
      const currency = o.currency === undefined ? undefined : String(o.currency).toUpperCase();
      if (style === 'currency') {
        if (!currency) throw new TypeError('Currency code is required with currency style.');
        const digits = currencyDigits[currency] ?? 2;
        if (inner.minimumFractionDigits === undefined && inner.maximumSignificantDigits === undefined) inner.minimumFractionDigits = Math.min(digits, inner.maximumFractionDigits ?? digits);
        if (inner.maximumFractionDigits === undefined && inner.maximumSignificantDigits === undefined) inner.maximumFractionDigits = Math.max(digits, inner.minimumFractionDigits);
      }
      if (style === 'percent' && inner.maximumFractionDigits === undefined && inner.maximumSignificantDigits === undefined) inner.maximumFractionDigits = Math.max(0, inner.minimumFractionDigits ?? 0);
      const f = Reflect.construct(t, [l, inner], nt);
      numberStyles.set(f, { style, currency, display: o.currencyDisplay || 'symbol', unit: o.unit, unitDisplay: o.unitDisplay || 'short' });
      return f;
    }
    const numberStyles = new WeakMap();
    const currencyDigits = { JPY: 0, KRW: 0, VND: 0, CLP: 0, ISK: 0, HUF: 2, BHD: 3, KWD: 3, JOD: 3, OMR: 3, TND: 3 };
    const currencySymbols = { USD: '$', EUR: '€', GBP: '£', JPY: '¥', CNY: 'CN¥', RUB: '₽', UAH: '₴', INR: '₹', KRW: '₩', BRL: 'R$', CAD: 'CA$', AUD: 'A$', TRY: '₺', ILS: '₪', PLN: 'zł', KZT: '₸' };
    const symbolAfter = l => /^(ru|uk|be|kk|de|fr|es|it|pl|cs|sk|fi|sv|nb|da|pt|nl|tr|hu|ro|bg|el|lt|lv|et|hr|sl|sr)\b/i.test(String(Array.isArray(l) ? l[0] : l || ''));
    {
      const NF = G.Intl.NumberFormat;
      const proto = NF && NF.prototype;
      const getter = proto && Object.getOwnPropertyDescriptor(proto, 'format');
      const plain = getter && getter.get;
      const resolved = proto && proto.resolvedOptions;
      if (plain && resolved) {
        const decorate = (f, v) => {
          const st = numberStyles.get(f) || { style: 'decimal' };
          const n = typeof v === 'bigint' ? v : Number(v);
          if (typeof n === 'number' && !Number.isFinite(n)) {
            const body = Number.isNaN(n) ? 'NaN' : '∞';
            return [n < 0 ? ['minusSign', '-'] : null, [Number.isNaN(n) ? 'nan' : 'infinity', body]].filter(Boolean);
          }
          const base = plain.call(f)(st.style === 'percent' && typeof n === 'number' ? n * 100 : n);
          const parts = [];
          const locale = resolved.call(f).locale;
          const dec = (plain.call(new NF(locale, { minimumFractionDigits: 1 }))(1.5).match(/1(.)5/) || [, '.'])[1];
          for (const m of base.matchAll(/(\d+)|([^\d])/g)) {
            if (m[1] !== undefined) {
              const prev = parts[parts.length - 1];
              parts.push([prev && prev[0] === 'decimal' ? 'fraction' : 'integer', m[1]]);
            } else if (m[2] === '-' || m[2] === '−') parts.push(['minusSign', m[2]]);
            else if (m[2] === '+') parts.push(['plusSign', m[2]]);
            else if (m[2] === dec) parts.push(['decimal', m[2]]);
            else if (/[\s,.'  ]/.test(m[2]) && parts.length && parts[parts.length - 1][0] === 'integer') parts.push(['group', m[2]]);
            else if (parts.length && parts[parts.length - 1][0] === 'literal') parts[parts.length - 1][1] += m[2];
            else parts.push(['literal', m[2]]);
          }
          // fractions after the first decimal stay fractions; groups need a following integer
          for (let i = 0; i < parts.length; i++) if (parts[i][0] === 'integer' && parts.slice(0, i).some(p => p[0] === 'decimal')) parts[i][0] = 'fraction';
          if (st.style === 'percent') parts.push(...(symbolAfter(locale) ? [['literal', ' ']] : []), ['percentSign', '%']);
          if (st.style === 'currency') {
            const sym = st.display === 'code' ? st.currency : st.display === 'name' ? st.currency : (currencySymbols[st.currency] || st.currency);
            const sign = parts[0] && parts[0][0] === 'minusSign' ? [parts.shift()] : [];
            if (symbolAfter(locale) || st.display === 'name') parts.push(['literal', ' '], ['currency', sym]);
            else parts.unshift(['currency', sym], ...(sym.length > 1 && /^[A-Z]+$/.test(sym) ? [['literal', ' ']] : []));
            parts.unshift(...sign);
          }
          if (st.style === 'unit' && st.unit) parts.push(['literal', st.unitDisplay === 'narrow' ? '' : ' '], ['unit', unitLabel(st.unit, st.unitDisplay)]);
          return parts;
        };
        const unitLabel = (u, d) => ({ kilometer: 'km', meter: 'm', centimeter: 'cm', kilogram: 'kg', gram: 'g', second: d === 'long' ? 'seconds' : 'sec', minute: d === 'long' ? 'minutes' : 'min', hour: d === 'long' ? 'hours' : 'hr', day: d === 'long' ? 'days' : 'days', byte: 'byte', kilobyte: 'kB', megabyte: 'MB', gigabyte: 'GB', percent: '%', 'kilometer-per-hour': 'km/h', celsius: '°C', fahrenheit: '°F', liter: 'L' })[u] || u;
        const bound = new WeakMap();
        Object.defineProperty(proto, 'format', { configurable: true, get() {
          let b = bound.get(this);
          if (!b) { const self = this; b = v => decorate(self, v).map(p => p[1]).join(''); bound.set(this, b); }
          return b;
        } });
        proto.formatToParts = { formatToParts(v) { return decorate(this, v).map(([type, value]) => ({ type, value })); } }.formatToParts;
        proto.resolvedOptions = { resolvedOptions() {
          const r = resolved.call(this);
          const st = numberStyles.get(this);
          if (st && st.style !== 'decimal') {
            r.style = st.style;
            if (st.style === 'currency') { r.currency = st.currency; r.currencyDisplay = st.display; r.currencySign = 'standard'; }
            if (st.style === 'unit') { r.unit = st.unit; r.unitDisplay = st.unitDisplay; }
          }
          return r;
        } }.resolvedOptions;
        const toLocale = Number.prototype.toLocaleString;
        Number.prototype.toLocaleString = { toLocaleString(l, o) { return new G.Intl.NumberFormat(l, o).format(Number(this)); } }.toLocaleString;
        void toLocale;
      }
    }
    const patch = (proto, name, at) => {
      const f = proto[name];
      if (typeof f !== 'function') return;
      proto[name] = { [name](...a) { a[at] = loc(a[at]); return f.apply(this, a); } }[name];
    };
    for (const n of ['toLocaleString', 'toLocaleDateString', 'toLocaleTimeString']) {
      const f = Date.prototype[n];
      Date.prototype[n] = { [n](l, o) { return f.call(this, loc(l), o && o.timeZone !== undefined || zone === undefined ? o : { ...(o || {}), timeZone: zone }); } }[n];
    }
    patch(Array.prototype, 'toLocaleString', 0);
    if (typeof BigInt === 'function') patch(BigInt.prototype, 'toLocaleString', 0);
    patch(String.prototype, 'localeCompare', 1);
    patch(String.prototype, 'toLocaleLowerCase', 0);
    patch(String.prototype, 'toLocaleUpperCase', 0);
  }

  // stubs: observers never fire, media queries never match, nothing is measured
  const Observer = class { constructor(cb) { this.cb = cb; } observe() {} unobserve() {} disconnect() {} takeRecords() { return []; } };
  // ----- stylesheets scripts add, and the CSSOM -------------------------------
  {
    // the rules a script inserted into a <style>, by element (layout reads them via __css_set)
    const rulesOf = new WeakMap();
    const loaded = new WeakSet();
    const initialLinks = new Set(__dom_query(0, 'link', true));
    class CSSRule { constructor(text, sheet) { this.cssText = text; this.parentStyleSheet = sheet; this.type = /^\s*@media/i.test(text) ? 4 : /^\s*@font-face/i.test(text) ? 5 : /^\s*@keyframes/i.test(text) ? 7 : 1; const m = /^([^{]*)\{/.exec(text); this.selectorText = m ? m[1].trim() : ''; } }
    G.CSSRule = CSSRule;
    G.CSSStyleRule = CSSRule;
    G.CSSStyleSheet = class CSSStyleSheet {
      constructor(owner = null) { this.ownerNode = owner; this.disabled = false; this.__rules = owner ? (rulesOf.get(owner) || []) : []; if (owner) rulesOf.set(owner, this.__rules); }
      get cssRules() { const r = this.__rules.map(t => new CSSRule(t, this)); r.item = i => r[i] ?? null; return r; }
      get rules() { return this.cssRules; }
      get href() { return this.ownerNode && this.ownerNode.localName === 'link' ? this.ownerNode.href : null; }
      get type() { return 'text/css'; }
      get media() { return { mediaText: this.ownerNode ? this.ownerNode.getAttribute('media') || '' : '', length: 0 }; }
      __sync() { if (this.ownerNode) __css_set(this.ownerNode.__sepiaNode, this.__rules.join('\n')); }
      insertRule(rule, index = 0) {
        if (index < 0 || index > this.__rules.length) throw new DOMException('Index out of range', 'IndexSizeError');
        this.__rules.splice(index, 0, String(rule)); this.__sync(); return index;
      }
      deleteRule(index) { if (index < 0 || index >= this.__rules.length) throw new DOMException('Index out of range', 'IndexSizeError'); this.__rules.splice(index, 1); this.__sync(); }
      addRule(sel, body, index) { return this.insertRule(`${sel}{${body}}`, index ?? this.__rules.length); }
      removeRule(i) { this.deleteRule(i ?? 0); }
      replaceSync(text) { this.__rules = [String(text)]; this.__sync(); }
      replace(text) { this.replaceSync(text); return Promise.resolve(this); }
    };
    Object.defineProperty(classes.Style.prototype, 'sheet', { get() { return this.isConnected ? new G.CSSStyleSheet(this) : null; }, configurable: true });
    Object.defineProperty(classes.Link.prototype, 'sheet', { get() { return this.isConnected && /\bstylesheet\b/i.test(this.getAttribute('rel') || '') ? new G.CSSStyleSheet(this) : null; }, configurable: true });
    Object.defineProperty(Document.prototype, 'styleSheets', { get() { const r = this.querySelectorAll('style, link[rel~=stylesheet]').map(e => e.sheet).filter(Boolean); r.item = i => r[i] ?? null; return r; }, configurable: true });
    // constructed sheets a page adopts (Lit, Shoelace…): their rules apply to the whole page here
    const adoptedHost = () => { let h = document.getElementById('__sepia_adopted'); if (!h) { h = document.createElement('style'); h.id = '__sepia_adopted'; (document.head || document.documentElement).append(h); } return h; };
    let adopted = [];
    Object.defineProperty(Document.prototype, 'adoptedStyleSheets', {
      get() { return adopted; },
      set(list) { adopted = [...list]; const h = adoptedHost(); __css_set(h.__sepiaNode, adopted.map(s => s.__rules.join('\n')).join('\n')); },
      configurable: true,
    });
    const linkIn = el => {
      if (loaded.has(el) || initialLinks.has(el.__sepiaNode)) return;
      if (!/\bstylesheet\b/i.test(el.getAttribute('rel') || '') || !el.getAttribute('href')) return;
      loaded.add(el);
      const href = el.href;
      const id = __resource_start(href);
      if (id === null) { G.setTimeout(() => el.dispatchEvent(new Event('error')), 0); return; }
      G.__sepia_await(id, row => {
        if (row[1] === null || row[1] >= 400) { el.dispatchEvent(new Event('error')); return; }
        __css_set(el.__sepiaNode, __css_absolute(row[2], row[3] || href));
        el.dispatchEvent(new Event('load'));
      });
    };
    // <link rel=preload as=style onload="this.rel='stylesheet'">: the preload "loads", and
    // turning it into a stylesheet applies it
    const preloaded = new WeakSet();
    const preloadIn = el => {
      if (preloaded.has(el) || !/\bpreload\b/i.test(el.getAttribute('rel') || '') || !el.getAttribute('href')) return;
      preloaded.add(el);
      G.setTimeout(() => { el.dispatchEvent(new Event(el.getAttribute('as') === 'style' && __css_fetch(el.href) === null ? 'error' : 'load')); }, 0);
    };
    const linkSeen = el => { linkIn(el); preloadIn(el); };
    for (const l of __dom_query(0, 'link', true)) { const el = wrap(l); if (el && el.isConnected) preloadIn(el); }
    const insert = G.__dom_insert, setAttr = G.__dom_set_attr;
    G.__dom_insert = (p, c, ref) => {
      const nodes = __dom_kind(c) === 11 ? __dom_children(c) : [c];
      const r = insert(p, c, ref);
      for (const n of nodes) {
        if (__dom_kind(n) !== 1) continue;
        for (const l of __dom_tag(n) === 'link' ? [n] : __dom_query(n, 'link', true)) { const el = wrap(l); if (el && el.isConnected) linkSeen(el); }
      }
      return r;
    };
    G.__dom_set_attr = (id, n, v) => {
      const r = setAttr(id, n, v);
      if (__dom_tag(id) === 'link' && /^(rel|href)$/i.test(n)) {
        const el = wrap(id);
        if (el && el.isConnected) { initialLinks.delete(id); loaded.delete(el); linkSeen(el); }
      }
      return r;
    };
  }

  // ----- scripts the page inserts run, as in a browser ------------------------
  {
    // scripts already in the document were run by the loader (or are not scripts to run)
    const started = new WeakSet();
    const startedIds = new Set(__dom_query(0, 'script', true));
    const runnable = el => {
      const type = (el.getAttribute('type') || '').trim().toLowerCase();
      if (type === 'module') return 'module';
      if (type === '' || type.includes('javascript') || type === 'text/ecmascript' || type === 'application/ecmascript') return el.hasAttribute('nomodule') ? null : 'classic';
      return null;
    };
    const fire = (el, type) => { try { el.dispatchEvent(new Event(type)); } catch (e) { log('script ' + type + ' handler', e); } };
    const prepare = el => {
      if (started.has(el) || startedIds.has(el.__sepiaNode)) return;
      started.add(el);
      const kind = runnable(el);
      if (!kind) return;
      const src = el.getAttribute('src');
      if (src === null) {
        const code = el.textContent;
        if (!code.trim()) return;
        if (kind === 'module') { log('an inline module script inserted by a script is not run'); return; }
        const prev = G.__currentScript; G.__currentScript = el;
        __script_eval(code, 'inserted script');
        G.__currentScript = prev;
        return;
      }
      // a data: URL needs no trip
      const data = /^\s*data:([^,]*),(.*)$/s.exec(src);
      if (data) {
        if (kind === 'module') { G.setTimeout(() => { import(src.trim()).then(() => fire(el, 'load'), e => { log('module data:', e); fire(el, 'error'); }); }, 0); return; }
        let code;
        try { code = /;base64$/i.test(data[1]) ? new G.TextDecoder().decode(Uint8Array.from(atob(decodeURIComponent(data[2])), c => c.charCodeAt(0))) : decodeURIComponent(data[2]); } catch (e) { G.setTimeout(() => fire(el, 'error'), 0); return; }
        G.setTimeout(() => { const prev = G.__currentScript; G.__currentScript = el; __script_eval(code, 'data: script'); G.__currentScript = prev; fire(el, 'load'); }, 0);
        return;
      }
      let url;
      try { url = new URL(src, document.baseURI).href; } catch (e) { G.setTimeout(() => fire(el, 'error'), 0); return; }
      if (kind === 'module') {
        G.setTimeout(() => { import(url).then(() => fire(el, 'load'), e => { log('module ' + url, e); fire(el, 'error'); }); }, 0);
        return;
      }
      // async, like a script added by script: fetched in the background, run
      // as soon as it arrives
      const run = row => {
        if (!row || row[1] === null || row[1] >= 400) { fire(el, 'error'); return; }
        const prev = G.__currentScript; G.__currentScript = el;
        __script_eval(row[2], url);
        G.__currentScript = prev;
        fire(el, 'load');
      };
      const id = __resource_start(url);
      if (id === null) { G.setTimeout(() => fire(el, 'error'), 0); return; }
      G.__sepia_await(id, run);
    };
    const scan = id => {
      if (__dom_kind(id) !== 1 && __dom_kind(id) !== 11) return;
      const els = __dom_tag(id) === 'script' ? [id] : __dom_query(id, 'script', true);
      for (const s of els) { const el = wrap(s); if (el && el.isConnected) prepare(el); }
    };
    const insert = G.__dom_insert, setHtml = G.__dom_set_html;
    G.__dom_insert = (p, c, ref) => {
      const nodes = __dom_kind(c) === 11 ? __dom_children(c) : [c];
      const r = insert(p, c, ref);
      for (const n of nodes) scan(n);
      return r;
    };
    // document.write's scripts do run (they are parser-inserted)
    G.__sepia_run_written = id => { for (const s of [id, ...__dom_query(id, 'script', true)]) { const el = wrap(s); if (el && el.localName === 'script') { started.delete(el); startedIds.delete(s); prepare(el); } } };
    // markup set through innerHTML never runs its scripts
    G.__dom_set_html = (id, v) => {
      const r = setHtml(id, v);
      for (const s of __dom_query(id, 'script', true)) { const el = wrap(s); if (el) started.add(el); }
      return r;
    };
  }

  // ----- MutationObserver: records from every DOM change, delivered as a microtask
  {
    const registrations = [];   // { observer, target (id), options }
    const pending = new Set();  // observers with records
    let scheduled = false;
    const nativeQueue = fn => nativeThen.call(nativeResolved, fn);
    const ancestry = id => { const out = []; for (let n = id; n !== undefined && n !== null && n >= 0; n = __dom_parent(n)) out.push(n); return out; };
    const deliver = () => {
      scheduled = false;
      for (const observer of [...pending]) {
        pending.delete(observer);
        const records = observer.__records.splice(0);
        if (records.length) { try { observer.__cb.call(observer, records, observer); } catch (e) { log('MutationObserver callback', e); } }
      }
    };
    // one change at node `target`: who wants to hear about it
    const queue = (target, make, type, name, oldValue) => {
      if (!registrations.length) return;
      const chain = ancestry(target);
      const interested = new Map();
      for (const r of registrations) {
        const depth = chain.indexOf(r.target);
        if (depth < 0 || (depth > 0 && !r.options.subtree)) continue;
        const o = r.options;
        if (type === 'attributes' && (!o.attributes || (o.attributeFilter && !o.attributeFilter.includes(name)))) continue;
        if (type === 'characterData' && !o.characterData) continue;
        if (type === 'childList' && !o.childList) continue;
        const wantsOld = (type === 'attributes' && o.attributeOldValue) || (type === 'characterData' && o.characterDataOldValue);
        if (!interested.has(r.observer) || wantsOld) interested.set(r.observer, wantsOld);
      }
      if (!interested.size) return;
      const base = make();
      for (const [observer, wantsOld] of interested) {
        observer.__records.push(Object.assign(new G.MutationRecord(), base, { type, target: wrap(target), attributeName: name ?? null, oldValue: wantsOld ? oldValue ?? null : null }));
        pending.add(observer);
      }
      if (!scheduled) { scheduled = true; nativeQueue(deliver); }
    };
    const siblings = (parent, first, last) => {
      const kids = __dom_children(parent);
      const i = kids.indexOf(first), j = kids.indexOf(last);
      return { previousSibling: i > 0 ? wrap(kids[i - 1]) : null, nextSibling: j >= 0 && j + 1 < kids.length ? wrap(kids[j + 1]) : null };
    };
    const childList = (parent, added, removed, around) => queue(parent, () => ({ addedNodes: nodeList(added.map(wrap)), removedNodes: nodeList(removed.map(wrap)), ...around }), 'childList');
    const { __dom_insert: insert, __dom_remove: remove, __dom_set_html: setHtml, __dom_clear: clear, __dom_set_attr: setAttr, __dom_remove_attr: removeAttr, __dom_set_text: setText } = G;
    G.__dom_insert = (p, c, ref) => {
      if (!registrations.length) return insert(p, c, ref);
      const moved = __dom_kind(c) === 11 ? __dom_children(c) : [c];
      const from = __dom_kind(c) === 11 ? -1 : __dom_parent(c);
      if (from !== undefined && from !== null && from >= 0) { const around = siblings(from, c, c); childList(from, [], [c], around); }
      const r = insert(p, c, ref);
      if (moved.length) childList(p, moved, [], siblings(p, moved[0], moved[moved.length - 1]));
      return r;
    };
    G.__dom_remove = id => {
      const parent = registrations.length ? __dom_parent(id) : -1;
      const around = parent !== undefined && parent !== null && parent >= 0 ? siblings(parent, id, id) : null;
      const r = remove(id);
      if (around) childList(parent, [], [id], around);
      return r;
    };
    G.__dom_set_html = (id, v) => {
      const old = registrations.length ? __dom_children(id) : [];
      const r = setHtml(id, v);
      if (registrations.length) childList(id, __dom_children(id), old, { previousSibling: null, nextSibling: null });
      return r;
    };
    G.__dom_clear = id => {
      const old = registrations.length ? __dom_children(id) : [];
      const r = clear(id);
      if (old.length) childList(id, [], old, { previousSibling: null, nextSibling: null });
      return r;
    };
    G.__dom_set_attr = (id, n, v) => { const old = registrations.length ? __dom_get_attr(id, n) : null; const r = setAttr(id, n, v); queue(id, () => ({}), 'attributes', String(n).toLowerCase(), old); return r; };
    G.__dom_remove_attr = (id, n) => {
      if (!registrations.length) return removeAttr(id, n);
      const old = __dom_get_attr(id, n); const r = removeAttr(id, n);
      if (old !== null) queue(id, () => ({}), 'attributes', String(n).toLowerCase(), old);
      return r;
    };
    G.__dom_set_text = (id, v) => {
      if (!registrations.length) return setText(id, v);
      const kind = __dom_kind(id);
      if (kind === 3 || kind === 8) { const old = __dom_text(id); const r = setText(id, v); queue(id, () => ({}), 'characterData', null, old); return r; }
      const old = __dom_children(id); const r = setText(id, v);
      childList(id, __dom_children(id), old, { previousSibling: null, nextSibling: null });
      return r;
    };
    G.MutationRecord = class MutationRecord { constructor() { Object.assign(this, { type: '', target: null, addedNodes: nodeList([]), removedNodes: nodeList([]), previousSibling: null, nextSibling: null, attributeName: null, attributeNamespace: null, oldValue: null }); } };
    G.MutationObserver = class MutationObserver {
      constructor(cb) { if (typeof cb !== 'function') throw new TypeError('MutationObserver needs a callback'); this.__cb = cb; this.__records = []; }
      observe(target, options = {}) {
        if (!target || target.__sepiaNode === undefined) throw new TypeError("Failed to execute 'observe': parameter 1 is not of type 'Node'.");
        const o = { ...options };
        if (o.attributeOldValue || o.attributeFilter) o.attributes ??= true;
        if (o.characterDataOldValue) o.characterData ??= true;
        if (!o.childList && !o.attributes && !o.characterData) throw new TypeError("The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.");
        if (o.attributeFilter) o.attributeFilter = [...o.attributeFilter].map(a => String(a).toLowerCase());
        const existing = registrations.find(r => r.observer === this && r.target === target.__sepiaNode);
        if (existing) existing.options = o; else registrations.push({ observer: this, target: target.__sepiaNode, options: o });
      }
      disconnect() { for (let i = registrations.length - 1; i >= 0; i--) if (registrations[i].observer === this) registrations.splice(i, 1); this.__records = []; pending.delete(this); }
      takeRecords() { pending.delete(this); return this.__records.splice(0); }
    };
    G.WebKitMutationObserver = G.MutationObserver;
  }
  G.PerformanceObserver = Observer;
  // size observers: told the element's box once observed, and again when it changes size
  {
    const rectFor = el => { const r = isNode(el) ? __dom_rect(el.__sepiaNode) : null; return r ? new G.DOMRect(r[0] - (G.scrollX || 0), r[1] - (G.scrollY || 0), r[2], r[3]) : new G.DOMRect(); };
    const resizeObservers = new Set();
    G.ResizeObserver = class ResizeObserver {
      constructor(cb) { if (typeof cb !== 'function') throw new TypeError("Failed to construct 'ResizeObserver': parameter 1 is not of type 'ResizeObserverCallback'."); this.__cb = cb; this.__targets = new Map(); }
      observe(el) { if (!isNode(el)) throw new TypeError("Failed to execute 'observe' on 'ResizeObserver': parameter 1 is not of type 'Element'."); this.__targets.set(el, null); resizeObservers.add(this); this.__schedule(); startPolling(); }
      unobserve(el) { this.__targets.delete(el); }
      disconnect() { this.__targets.clear(); resizeObservers.delete(this); }
      __schedule() { if (this.__queued) return; this.__queued = true; setTimeout(() => { this.__queued = false; this.__check(); }, 0); }
      __check() {
        const entries = [];
        for (const [el, last] of this.__targets) {
          const r = rectFor(el);
          if (last && last[0] === r.width && last[1] === r.height) continue;
          this.__targets.set(el, [r.width, r.height]);
          const size = [{ inlineSize: r.width, blockSize: r.height }];
          entries.push({ target: el, contentRect: new G.DOMRect(0, 0, r.width, r.height), borderBoxSize: size, contentBoxSize: size, devicePixelContentBoxSize: size });
        }
        if (entries.length) { try { this.__cb(entries, this); } catch (e) { log('ResizeObserver', e); } }
      }
    };
    G.addEventListener('resize', () => { for (const o of resizeObservers) o.__schedule(); });
    // as a browser checks after each frame's layout: look again while anything is observed
    let polling = false;
    const poll = () => {
      let any = false;
      for (const o of resizeObservers) if (o.__targets.size) { any = true; o.__check(); }
      polling = any;
      if (any) setTimeout(poll, 200);
    };
    const startPolling = () => { if (!polling) { polling = true; setTimeout(poll, 200); } };
    // lazy loaders: everything counts as visible at once (the page is not scrolled for them)
    G.IntersectionObserver = class IntersectionObserver {
      constructor(cb, options = {}) {
        if (typeof cb !== 'function') throw new TypeError("Failed to construct 'IntersectionObserver': parameter 1 is not of type 'IntersectionObserverCallback'.");
        this.__cb = cb; this.root = options.root || null; this.rootMargin = options.rootMargin || '0px 0px 0px 0px';
        const t = options.threshold; this.thresholds = Array.isArray(t) ? t : [t === undefined ? 0 : t];
        this.__targets = new Set(); this.__records = [];
      }
      observe(el) {
        if (!isNode(el)) throw new TypeError("Failed to execute 'observe' on 'IntersectionObserver': parameter 1 is not of type 'Element'.");
        if (this.__targets.has(el)) return;
        this.__targets.add(el);
        setTimeout(() => {
          if (!this.__targets.has(el)) return;
          const r = rectFor(el);
          const rec = { target: el, isIntersecting: true, intersectionRatio: 1, boundingClientRect: r, intersectionRect: r, rootBounds: new G.DOMRect(0, 0, G.innerWidth, G.innerHeight), time: performance.now(), isVisible: true };
          try { this.__cb([rec], this); } catch (e) { log('IntersectionObserver', e); }
        }, 0);
      }
      unobserve(el) { this.__targets.delete(el); }
      disconnect() { this.__targets.clear(); }
      takeRecords() { return this.__records.splice(0); }
    };
    G.IntersectionObserverEntry = class IntersectionObserverEntry {};
    G.ResizeObserverEntry = class ResizeObserverEntry {};
  }
  // media queries, evaluated as our CSS does, against the window's size
  {
    const lists = [];
    class MediaQueryList extends EventTarget {
      constructor(q) { super(); this.media = String(q); this.onchange = null; this.__was = this.matches; }
      get matches() { return __media_matches(this.media, G.innerWidth, G.innerHeight); }
      addListener(f) { this.addEventListener('change', f); }
      removeListener(f) { this.removeEventListener('change', f); }
    }
    G.MediaQueryList = MediaQueryList;
    G.matchMedia = q => { const l = new MediaQueryList(q); lists.push(new WeakRef(l)); return l; };
    G.addEventListener('resize', () => {
      for (let i = lists.length - 1; i >= 0; i--) {
        const l = lists[i].deref();
        if (!l) { lists.splice(i, 1); continue; }
        const now = l.matches;
        if (now === l.__was) continue;
        l.__was = now;
        const e = new Event('change'); e.matches = now; e.media = l.media;
        if (typeof l.onchange === 'function') { try { l.onchange(e); } catch (err) { log('matchMedia', err); } }
        l.dispatchEvent(e);
      }
    });
  }
  // the style our engine computed (layout's own view), with the inline
  // style's other properties and custom properties on top
  G.getComputedStyle = (el, pseudo) => {
    if (!isNode(el) || el.nodeType !== 1) throw new TypeError("Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.");
    const values = new Map();
    const pairs = pseudo ? null : __computed_style(el.__sepiaNode);
    if (pairs) for (let i = 0; i < pairs.length; i += 2) values.set(pairs[i], pairs[i + 1]);
    else values.set('display', 'none');
    const inline = el.style;
    for (let i = 0; i < inline.length; i++) { const k = inline[i]; if (!values.has(k) || k.startsWith('--')) values.set(k, inline.getPropertyValue(k)); }
    const kebab = k => k.replace(/[A-Z]/g, m => '-' + m.toLowerCase());
    const decl = {
      getPropertyValue: k => { k = String(k); return values.get(k) ?? values.get(k.toLowerCase()) ?? ''; },
      getPropertyPriority: () => '',
      setProperty() { throw new DOMException('read-only', 'NoModificationAllowedError'); },
      removeProperty() { throw new DOMException('read-only', 'NoModificationAllowedError'); },
      item: i => [...values.keys()][i] || '',
      get length() { return values.size; },
      get cssText() { return ''; },
    };
    return new Proxy(decl, {
      get(t, k) {
        if (typeof k !== 'string' || k in t) return t[k];
        if (/^\d+$/.test(k)) return [...values.keys()][+k];
        return values.get(kebab(k)) ?? (k === 'cssFloat' ? values.get('float') : '') ?? '';
      },
    });
  };
  G.CSS = { escape: s => String(s).replace(/([^\w-])/g, '\\$1'), supports: (p, v) => { const t = String(v === undefined ? p : p + ':' + v).trim(); return !/^-(ms|o)-|^selector\(.*:has|^-webkit-box\b/i.test(t); } };

  // the same answers for everybody (fingerprinting)
  G.navigator = {
    // the same as the request headers: the browser and country chosen in the settings
    userAgent: persona.userAgent, appName: 'Netscape', appVersion: persona.appVersion, platform: persona.platform,
    vendor: persona.vendor, vendorSub: '', product: 'Gecko', productSub: persona.productSub,
    ...(persona.oscpu ? { oscpu: persona.oscpu, buildID: '20181001000000' } : {}),
    language: persona.language, languages: Object.freeze(persona.languages.slice()), hardwareConcurrency: 2, deviceMemory: 4, maxTouchPoints: 0,
    cookieEnabled: true, onLine: true, doNotTrack: null, webdriver: false, pdfViewerEnabled: false,
    plugins: [], mimeTypes: [], clipboard: { writeText: () => Promise.reject(new Error('blocked')), readText: () => Promise.reject(new Error('blocked')) },
    // a POST sent in the background; the answer is never read
    sendBeacon: (url, data) => { try { sendLater(String(url), 'POST', data ?? '', data instanceof G.Blob && data.type ? { 'content-type': data.type } : null, () => {}, true); return true; } catch (e) { return false; } }, vibrate: () => false, share: () => Promise.reject(new Error('blocked')),
    serviceWorker: undefined, geolocation: undefined, mediaDevices: undefined,
  };
  if (persona.hints) {
    const h = persona.hints;
    const brands = [{ brand: 'Not.A/Brand', version: '99' }, { brand: 'Chromium', version: h.major }, { brand: h.brand, version: h.major }];
    G.navigator.userAgentData = {
      brands, mobile: false, platform: h.platform,
      getHighEntropyValues: keys => Promise.resolve(Object.fromEntries([['brands', brands], ['mobile', false], ['platform', h.platform], ...[].concat(keys || []).map(k => [k, ({ architecture: 'x86', bitness: '64', model: '', platformVersion: h.platform === 'Windows' ? '15.0.0' : '15.0.0', uaFullVersion: h.major + '.0.0.0', fullVersionList: brands.map(b => ({ brand: b.brand, version: b.version + '.0.0.0' })), wow64: false })[k]]).filter(([, v]) => v !== undefined)])),
      toJSON() { return { brands, mobile: false, platform: h.platform }; },
    };
  }
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
  {
    // User Timing works on the same coarse clock; there are no resource or paint entries
    const now = () => Math.floor((Date.now() - start) / 100) * 100;
    const origin = Math.floor(start / 100) * 100;
    let entries = [];
    const add = (e) => { entries.push(e); return e; };
    const entry = (name, entryType, startTime, duration, detail) => ({ name: String(name), entryType, startTime, duration, detail: detail ?? null, toJSON() { return { name: this.name, entryType, startTime, duration }; } });
    const at = (m) => typeof m === 'number' ? m : m === undefined ? now() : (entries.filter(e => e.name === m).pop() || { startTime: 0 }).startTime;
    G.performance = {
      now, timeOrigin: origin,
      mark: (name, o = {}) => add(entry(name, 'mark', o.startTime ?? now(), 0, o.detail)),
      measure(name, a, b) {
        let from, to;
        if (a && typeof a === 'object') { from = at(a.start); to = a.end !== undefined ? at(a.end) : a.duration !== undefined ? from + a.duration : now(); }
        else { from = a === undefined ? 0 : at(a); to = at(b); }
        return add(entry(name, 'measure', from, to - from, a && a.detail));
      },
      clearMarks: (n) => { entries = entries.filter(e => e.entryType !== 'mark' || (n !== undefined && e.name !== n)); },
      clearMeasures: (n) => { entries = entries.filter(e => e.entryType !== 'measure' || (n !== undefined && e.name !== n)); },
      clearResourceTimings() {}, setResourceTimingBufferSize() {},
      getEntries: () => entries.slice(),
      getEntriesByType: t => t === 'navigation' ? [G.performance.__nav] : entries.filter(e => e.entryType === t),
      getEntriesByName: (n, t) => entries.filter(e => e.name === n && (!t || e.entryType === t)),
      addEventListener() {}, removeEventListener() {},
      toJSON() { return { timeOrigin: origin }; },
    };
    // the page loads in no time: every phase at the origin
    const z = origin;
    G.performance.timing = { navigationStart: z, fetchStart: z, domainLookupStart: z, domainLookupEnd: z, connectStart: z, connectEnd: z, secureConnectionStart: z, requestStart: z, responseStart: z, responseEnd: z, domLoading: z, domInteractive: z, domContentLoadedEventStart: z, domContentLoadedEventEnd: z, domComplete: z, loadEventStart: z, loadEventEnd: z, redirectStart: 0, redirectEnd: 0, unloadEventStart: 0, unloadEventEnd: 0, toJSON() { return this; } };
    G.performance.navigation = { type: 0, redirectCount: 0, TYPE_NAVIGATE: 0, TYPE_RELOAD: 1, TYPE_BACK_FORWARD: 2 };
    G.performance.__nav = { name: location.href, entryType: 'navigation', startTime: 0, duration: 0, type: 'navigate', redirectCount: 0, domInteractive: 0, domContentLoadedEventStart: 0, domContentLoadedEventEnd: 0, domComplete: 0, loadEventStart: 0, loadEventEnd: 0, responseStart: 0, transferSize: 0, toJSON() { return this; } };
  }
  G.crypto = { getRandomValues: a => { for (let i = 0; i < a.length; i++) a[i] = Math.floor(Math.random() * 256); return a; }, randomUUID: () => 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, c => { const r = Math.random() * 16 | 0; return (c === 'x' ? r : (r & 3 | 8)).toString(16); }) };
  G.console = { log, info: log, warn: log, error: log, debug: () => {}, trace: () => {}, table: log, group: () => {}, groupEnd: () => {}, groupCollapsed: () => {}, time: () => {}, timeEnd: () => {}, assert: (c, ...a) => { if (!c) log('assert', ...a); }, dir: log, count: () => {} };
  G.reportError = e => { log('reportError', e); const ev = new Event('error'); ev.error = e; ev.message = String(e && e.message || e); G.dispatchEvent(ev); };
  // ----- structured clone (postMessage, IndexedDB, history state) ----------
  G.structuredClone = (value) => {
    const seen = new Map();
    const fail = v => { throw new DOMException(`${Object.prototype.toString.call(v)} could not be cloned.`, 'DataCloneError'); };
    const copy = v => {
      if (v === null || (typeof v !== 'object' && typeof v !== 'function')) return typeof v === 'symbol' ? fail(v) : v;
      if (typeof v === 'function') fail(v);
      if (seen.has(v)) return seen.get(v);
      let out;
      if (v instanceof Date) out = new Date(v.getTime());
      else if (v instanceof RegExp) out = new RegExp(v.source, v.flags);
      else if (v instanceof Boolean || v instanceof Number || v instanceof String) out = Object(v.valueOf());
      else if (v instanceof ArrayBuffer) out = v.slice(0);
      else if (ArrayBuffer.isView(v)) { const b = copy(v.buffer); out = v instanceof DataView ? new DataView(b, v.byteOffset, v.byteLength) : new v.constructor(b, v.byteOffset, v.length); }
      else if (v instanceof G.Blob) out = v instanceof G.File ? new G.File([v.__s], v.name, { type: v.type }) : new G.Blob([v.__s], { type: v.type });
      else if (v instanceof Error) { out = new (G[v.name] && G[v.name].prototype instanceof Error ? G[v.name] : Error)(v.message); if (v.stack) out.stack = v.stack; }
      else if (v instanceof NodeBase || v instanceof Promise || v instanceof WeakMap || v instanceof WeakSet) fail(v);
      else if (v instanceof Map) { out = new Map(); seen.set(v, out); for (const [k, x] of v) out.set(copy(k), copy(x)); return out; }
      else if (v instanceof Set) { out = new Set(); seen.set(v, out); for (const x of v) out.add(copy(x)); return out; }
      else if (Array.isArray(v)) { out = new Array(v.length); seen.set(v, out); for (const k of Object.keys(v)) out[k] = copy(v[k]); return out; }
      else { out = {}; seen.set(v, out); for (const k of Object.keys(v)) out[k] = copy(v[k]); return out; }
      seen.set(v, out);
      return out;
    };
    return copy(value);
  };

  // ----- IndexedDB, in memory (gone with the page's process) ---------------
  {
    const later = fn => G.setTimeout(fn, 0);
    const keyType = k => typeof k === 'number' ? (isNaN(k) ? null : 1) : k instanceof Date ? (isNaN(k.getTime()) ? null : 2) : typeof k === 'string' ? 3 : (k instanceof ArrayBuffer || ArrayBuffer.isView(k)) ? 4 : Array.isArray(k) ? 5 : null;
    const valid = k => { const t = keyType(k); return t !== null && (t !== 5 || k.every(valid)); };
    const bytes = k => new Uint8Array(k instanceof ArrayBuffer ? k : k.buffer, k instanceof ArrayBuffer ? 0 : k.byteOffset, k.byteLength);
    const cmp = (a, b) => {
      const ta = keyType(a), tb = keyType(b);
      if (ta !== tb) return ta < tb ? -1 : 1;
      switch (ta) {
        case 1: return a === b ? 0 : a < b ? -1 : 1;
        case 2: return cmp(a.getTime(), b.getTime());
        case 3: return a === b ? 0 : a < b ? -1 : 1;
        case 4: { const x = bytes(a), y = bytes(b); for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1; return cmp(x.length, y.length); }
        case 5: { for (let i = 0; i < Math.min(a.length, b.length); i++) { const c = cmp(a[i], b[i]); if (c) return c; } return cmp(a.length, b.length); }
      }
      return 0;
    };
    const dataError = m => new DOMException(m || 'Data provided to an operation does not meet requirements.', 'DataError');
    const checkKey = k => { if (!valid(k)) throw dataError('The parameter is not a valid key.'); return k instanceof Date ? new Date(k.getTime()) : Array.isArray(k) ? k.map(checkKey) : k; };
    const extract = (value, path) => {
      if (Array.isArray(path)) return path.map(p => extract(value, p));
      if (path === '') return value;
      let v = value;
      for (const part of path.split('.')) { if (v == null || typeof v !== 'object' && typeof v !== 'string' || !(part in Object(v))) return undefined; v = v[part]; }
      return v;
    };
    const inject = (value, path, key) => {
      const parts = path.split('.'); let v = value;
      for (const p of parts.slice(0, -1)) { if (v[p] === undefined) v[p] = {}; v = v[p]; }
      v[parts[parts.length - 1]] = key;
    };

    G.IDBKeyRange = class IDBKeyRange {
      constructor(lower, upper, lowerOpen, upperOpen) { this.lower = lower; this.upper = upper; this.lowerOpen = !!lowerOpen; this.upperOpen = !!upperOpen; }
      static only(v) { v = checkKey(v); return new IDBKeyRange(v, v, false, false); }
      static lowerBound(v, open) { return new IDBKeyRange(checkKey(v), undefined, open, true); }
      static upperBound(v, open) { return new IDBKeyRange(undefined, checkKey(v), true, open); }
      static bound(l, u, lo, uo) { l = checkKey(l); u = checkKey(u); if (cmp(l, u) > 0) throw dataError(); return new IDBKeyRange(l, u, lo, uo); }
      includes(k) {
        k = checkKey(k);
        if (this.lower !== undefined) { const c = cmp(k, this.lower); if (c < 0 || (c === 0 && this.lowerOpen)) return false; }
        if (this.upper !== undefined) { const c = cmp(k, this.upper); if (c > 0 || (c === 0 && this.upperOpen)) return false; }
        return true;
      }
    };
    const rangeOf = q => q === undefined || q === null ? null : q instanceof G.IDBKeyRange ? q : G.IDBKeyRange.only(q);
    const inRange = (range, k) => !range || range.includes(k);
    const names = list => { const a = [...list].sort(); a.contains = n => a.includes(n); a.item = i => a[i] ?? null; return a; };

    class IDBRequest extends EventTarget {
      constructor(source, transaction) { super(); this.source = source; this.transaction = transaction; this.readyState = 'pending'; this.__result = undefined; this.error = null; this.onsuccess = this.onerror = null; }
      get result() { return this.__result; }
      __deliver(kind, payload) {
        if (kind === 'success') { this.readyState = 'done'; this.__result = payload; }
        else { this.readyState = 'done'; this.error = payload; }
        const e = new Event(kind, { bubbles: kind === 'error', cancelable: kind === 'error' });
        Object.defineProperty(e, 'target', { value: this, configurable: true });
        try { this.dispatchEvent(e); } catch (err) { log('IndexedDB handler', String(err)); }
        if (kind === 'error' && !e.defaultPrevented && this.transaction) this.transaction.__abort(payload);
      }
    }
    class IDBOpenDBRequest extends IDBRequest { constructor() { super(null, null); this.onupgradeneeded = this.onblocked = null; } }

    // the data: name → { version, stores: Map(name → { keyPath, autoIncrement, next, records: [[key, value]] sorted, indexes: Map }) }
    const databases = new Map();

    class IDBTransaction extends EventTarget {
      constructor(db, scope, mode) {
        super(); this.db = db; this.mode = mode; this.__scope = scope; this.__pending = 0; this.__done = false; this.error = null;
        this.oncomplete = this.onerror = this.onabort = null; this.durability = 'default';
        this.__snapshot = new Map([...scope].map(n => [n, snapshotStore(db.__data.stores.get(n))]));
        this.__schedule();
      }
      get objectStoreNames() { return names(this.__scope); }
      objectStore(name) {
        if (!this.__scope.includes(name) || !this.db.__data.stores.has(name)) throw new DOMException(`No objectStore named ${name} in this database`, 'NotFoundError');
        return new IDBObjectStore(this, name);
      }
      __request(source, run) {
        if (this.__done) throw new DOMException('The transaction has finished.', 'TransactionInactiveError');
        const req = new IDBRequest(source, this);
        this.__pending++;
        later(() => {
          this.__pending--;
          if (this.__done) return;
          let result, error;
          try { result = run(); } catch (e) { error = e; }
          if (error) req.__deliver('error', error); else req.__deliver('success', result);
          this.__schedule();
        });
        return req;
      }
      __schedule() { later(() => { if (!this.__done && this.__pending === 0) this.__finish(); }); }
      __finish() {
        this.__done = true;
        const e = new Event('complete');
        this.dispatchEvent(e);
        if (this.__onfinish) this.__onfinish();
      }
      __abort(error) {
        if (this.__done) return;
        this.__done = true; this.error = error || null;
        for (const [n, snap] of this.__snapshot) if (this.db.__data.stores.has(n)) this.db.__data.stores.set(n, snap);
        if (this.mode === 'versionchange' && this.__oldData) { databases.set(this.db.name, this.__oldData); }
        const e = new Event('abort');
        this.dispatchEvent(e);
        if (this.__onfinish) this.__onfinish(true);
      }
      abort() { this.__abort(new DOMException('The transaction was aborted.', 'AbortError')); }
      commit() { if (this.__pending === 0) this.__finish(); }
    }
    const snapshotStore = s => s && ({ ...s, records: s.records.slice(), indexes: new Map([...s.indexes].map(([k, v]) => [k, { ...v }])) });

    const find = (records, key) => { let lo = 0, hi = records.length; while (lo < hi) { const mid = (lo + hi) >> 1; if (cmp(records[mid][0], key) < 0) lo = mid + 1; else hi = mid; } return lo; };

    class IDBObjectStore {
      constructor(tx, name) { this.transaction = tx; this.__name = name; }
      get __s() { return this.transaction.db.__data.stores.get(this.__name); }
      get name() { return this.__name; }
      get keyPath() { return this.__s.keyPath; }
      get autoIncrement() { return this.__s.autoIncrement; }
      get indexNames() { return names(this.__s.indexes.keys()); }
      __write() { if (this.transaction.mode === 'readonly') throw new DOMException('The transaction is read-only.', 'ReadOnlyError'); }
      __store(value, key, overwrite) {
        this.__write();
        const s = this.__s;
        value = G.structuredClone(value);
        if (s.keyPath !== null) {
          if (key !== undefined) throw dataError('The object store uses in-line keys and the key parameter was provided.');
          key = extract(value, s.keyPath);
          if (key === undefined && s.autoIncrement) { key = s.next; inject(value, s.keyPath, key); }
        } else if (key === undefined && s.autoIncrement) key = s.next;
        if (key === undefined) throw dataError('No key was provided.');
        key = checkKey(key);
        if (typeof key === 'number' && s.autoIncrement && key >= s.next) s.next = Math.floor(key) + 1;
        return { key, value, overwrite };
      }
      __apply({ key, value, overwrite }) {
        const s = this.__s, i = find(s.records, key);
        const exists = i < s.records.length && cmp(s.records[i][0], key) === 0;
        if (exists && !overwrite) throw new DOMException('Key already exists in the object store.', 'ConstraintError');
        for (const [n, ix] of s.indexes) if (ix.unique) {
          const v = extract(value, ix.keyPath);
          if (v !== undefined && valid(v) && s.records.some(([k, r]) => cmp(k, key) !== 0 && valid(extract(r, ix.keyPath)) && cmp(extract(r, ix.keyPath), v) === 0)) throw new DOMException(`Unable to add key to index '${n}': at least one key does not satisfy the uniqueness requirements.`, 'ConstraintError');
        }
        if (exists) s.records[i] = [key, value]; else s.records.splice(i, 0, [key, value]);
        return key;
      }
      put(value, key) { const w = this.__store(value, key, true); return this.transaction.__request(this, () => this.__apply(w)); }
      add(value, key) { const w = this.__store(value, key, false); return this.transaction.__request(this, () => this.__apply(w)); }
      __matching(query) { const range = rangeOf(query); return this.__s.records.filter(([k]) => inRange(range, k)); }
      get(query) { if (query === undefined) throw dataError(); return this.transaction.__request(this, () => { const r = this.__matching(query)[0]; return r ? G.structuredClone(r[1]) : undefined; }); }
      getKey(query) { return this.transaction.__request(this, () => { const r = this.__matching(query)[0]; return r ? r[0] : undefined; }); }
      getAll(query, count) { return this.transaction.__request(this, () => this.__matching(query).slice(0, count || undefined).map(r => G.structuredClone(r[1]))); }
      getAllKeys(query, count) { return this.transaction.__request(this, () => this.__matching(query).slice(0, count || undefined).map(r => r[0])); }
      count(query) { return this.transaction.__request(this, () => this.__matching(query).length); }
      delete(query) { this.__write(); const range = rangeOf(query); return this.transaction.__request(this, () => { this.__s.records = this.__s.records.filter(([k]) => !inRange(range, k)); }); }
      clear() { this.__write(); return this.transaction.__request(this, () => { this.__s.records = []; }); }
      openCursor(query, direction) { return openCursor(this, () => this.__matching(query).map(([k, v]) => [k, k, v]), direction, true); }
      openKeyCursor(query, direction) { return openCursor(this, () => this.__matching(query).map(([k, v]) => [k, k, v]), direction, false); }
      createIndex(name, keyPath, opts = {}) {
        if (this.transaction.mode !== 'versionchange') throw new DOMException('Not in a version change transaction.', 'InvalidStateError');
        if (this.__s.indexes.has(name)) throw new DOMException(`An index named '${name}' already exists.`, 'ConstraintError');
        this.__s.indexes.set(name, { keyPath, unique: !!opts.unique, multiEntry: !!opts.multiEntry });
        return this.index(name);
      }
      deleteIndex(name) { this.__s.indexes.delete(name); }
      index(name) { if (!this.__s.indexes.has(name)) throw new DOMException(`No index named '${name}'.`, 'NotFoundError'); return new IDBIndex(this, name); }
    }

    class IDBIndex {
      constructor(store, name) { this.objectStore = store; this.name = name; }
      get __ix() { return this.objectStore.__s.indexes.get(this.name); }
      get keyPath() { return this.__ix.keyPath; }
      get unique() { return this.__ix.unique; }
      get multiEntry() { return this.__ix.multiEntry; }
      // [indexKey, primaryKey, value] in index order
      __entries(query) {
        const ix = this.__ix, range = rangeOf(query), out = [];
        for (const [pk, v] of this.objectStore.__s.records) {
          const k = extract(v, ix.keyPath);
          const keys = ix.multiEntry && Array.isArray(k) ? [...new Map(k.filter(valid).map(x => [JSON.stringify(x), x])).values()] : valid(k) ? [k] : [];
          for (const key of keys) if (inRange(range, key)) out.push([key, pk, v]);
        }
        return out.sort((a, b) => cmp(a[0], b[0]) || cmp(a[1], b[1]));
      }
      __req(fn) { return this.objectStore.transaction.__request(this, fn); }
      get(query) { return this.__req(() => { const e = this.__entries(query)[0]; return e ? G.structuredClone(e[2]) : undefined; }); }
      getKey(query) { return this.__req(() => { const e = this.__entries(query)[0]; return e ? e[1] : undefined; }); }
      getAll(query, count) { return this.__req(() => this.__entries(query).slice(0, count || undefined).map(e => G.structuredClone(e[2]))); }
      getAllKeys(query, count) { return this.__req(() => this.__entries(query).slice(0, count || undefined).map(e => e[1])); }
      count(query) { return this.__req(() => this.__entries(query).length); }
      openCursor(query, direction) { return openCursor(this, () => this.__entries(query), direction, true); }
      openKeyCursor(query, direction) { return openCursor(this, () => this.__entries(query), direction, false); }
    }

    // cursors walk a snapshot of [key, primaryKey, value] taken when they open
    function openCursor(source, entries, direction = 'next', withValue) {
      const store = source instanceof IDBIndex ? source.objectStore : source;
      const tx = store.transaction;
      let list, pos = -1;
      const req = tx.__request(source, () => {
        list = entries();
        if (direction.startsWith('prev')) list.reverse();
        if (direction.endsWith('unique')) list = list.filter((e, i) => i === 0 || cmp(e[0], list[i - 1][0]) !== 0);
        return step(1);
      });
      const cursor = withValue ? new IDBCursorWithValue() : new IDBCursor();
      Object.assign(cursor, { source, direction, request: req });
      function step(n) { pos += n; if (pos >= list.length) return null; const [k, pk, v] = list[pos]; cursor.key = k; cursor.primaryKey = pk; if (withValue) cursor.value = G.structuredClone(v); return cursor; }
      const again = run => { tx.__pending++; req.readyState = 'pending'; later(() => { tx.__pending--; if (tx.__done) return; let r, err; try { r = run(); } catch (e) { err = e; } if (err) req.__deliver('error', err); else req.__deliver('success', r); tx.__schedule(); }); };
      cursor.continue = key => again(() => { if (key === undefined) return step(1); key = checkKey(key); const fwd = !direction.startsWith('prev'); do { pos++; } while (pos < list.length && (fwd ? cmp(list[pos][0], key) < 0 : cmp(list[pos][0], key) > 0)); return step(0); });
      cursor.advance = n => again(() => step(n));
      cursor.continuePrimaryKey = (key, pk) => again(() => { do { pos++; } while (pos < list.length && (cmp(list[pos][0], key) < 0 || (cmp(list[pos][0], key) === 0 && cmp(list[pos][1], pk) < 0))); return step(0); });
      cursor.update = value => { store.__write(); const w = store.keyPath !== null ? store.__store(value, undefined, true) : store.__store(value, cursor.primaryKey, true); return tx.__request(cursor, () => store.__apply(w)); };
      cursor.delete = () => { store.__write(); const pk = cursor.primaryKey; return tx.__request(cursor, () => { const s = store.__s; const i = find(s.records, pk); if (i < s.records.length && cmp(s.records[i][0], pk) === 0) s.records.splice(i, 1); }); };
      return req;
    }
    class IDBCursor {}
    class IDBCursorWithValue extends IDBCursor {}

    class IDBDatabase extends EventTarget {
      constructor(name) { super(); this.name = name; this.onversionchange = this.onclose = this.onabort = this.onerror = null; this.__closed = false; }
      get __data() { return databases.get(this.name); }
      get version() { return this.__data ? this.__data.version : 0; }
      get objectStoreNames() { return names(this.__data.stores.keys()); }
      createObjectStore(name, opts = {}) {
        if (!this.__upgrade) throw new DOMException('Not in a version change transaction.', 'InvalidStateError');
        if (this.__data.stores.has(name)) throw new DOMException(`Object store '${name}' already exists.`, 'ConstraintError');
        const keyPath = opts.keyPath === undefined || opts.keyPath === null ? null : opts.keyPath;
        this.__data.stores.set(name, { keyPath, autoIncrement: !!opts.autoIncrement, next: 1, records: [], indexes: new Map() });
        this.__upgrade.__scope.push(name);
        return new IDBObjectStore(this.__upgrade, name);
      }
      deleteObjectStore(name) { if (!this.__upgrade) throw new DOMException('Not in a version change transaction.', 'InvalidStateError'); this.__data.stores.delete(name); }
      transaction(storeNames, mode = 'readonly') {
        if (this.__closed) throw new DOMException('The database connection is closing.', 'InvalidStateError');
        const scope = [].concat(storeNames).map(String);
        for (const n of scope) if (!this.__data.stores.has(n)) throw new DOMException(`One of the specified object stores was not found: ${n}`, 'NotFoundError');
        if (!['readonly', 'readwrite'].includes(mode)) throw new TypeError(`Invalid transaction mode: ${mode}`);
        return new IDBTransaction(this, scope, mode);
      }
      close() { this.__closed = true; }
    }

    G.indexedDB = {
      open(name, version) {
        name = String(name);
        if (version !== undefined && (!(version >= 1) || version !== Math.floor(version))) throw new TypeError('The version must be a positive integer.');
        const req = new IDBOpenDBRequest();
        later(() => {
          const old = databases.get(name);
          const oldVersion = old ? old.version : 0;
          const newVersion = version === undefined ? Math.max(oldVersion, 1) : version;
          if (newVersion < oldVersion) { req.__deliver('error', new DOMException(`The requested version (${newVersion}) is less than the existing version (${oldVersion}).`, 'VersionError')); return; }
          const db = new IDBDatabase(name);
          if (newVersion === oldVersion) { req.__deliver('success', db); return; }
          const before = old ? { version: old.version, stores: new Map([...old.stores].map(([k, v]) => [k, snapshotStore(v)])) } : null;
          databases.set(name, { version: newVersion, stores: old ? old.stores : new Map() });
          const tx = new IDBTransaction(db, [...db.__data.stores.keys()], 'versionchange');
          tx.__oldData = before;
          db.__upgrade = tx;
          req.transaction = tx;
          tx.__onfinish = aborted => {
            db.__upgrade = null; req.transaction = null;
            if (aborted) { if (!before) databases.delete(name); req.__deliver('error', new DOMException('The upgrade was aborted.', 'AbortError')); }
            else req.__deliver('success', db);
          };
          req.readyState = 'done'; req.__result = db;
          const e = new Event('upgradeneeded');
          Object.assign(e, { oldVersion, newVersion });
          Object.defineProperty(e, 'target', { value: req, configurable: true });
          try { req.dispatchEvent(e); } catch (err) { log('IndexedDB upgrade', String(err)); tx.abort(); }
        });
        return req;
      },
      deleteDatabase(name) {
        const req = new IDBOpenDBRequest();
        later(() => { const old = databases.get(String(name)); databases.delete(String(name)); const e = { oldVersion: old ? old.version : 0, newVersion: null }; req.__deliver('success', undefined); void e; });
        return req;
      },
      databases: () => Promise.resolve([...databases].map(([name, d]) => ({ name, version: d.version }))),
      cmp: (a, b) => cmp(checkKey(a), checkKey(b)),
    };
    Object.assign(G, { IDBFactory: Object, IDBDatabase, IDBTransaction, IDBObjectStore, IDBIndex, IDBCursor, IDBCursorWithValue, IDBRequest, IDBOpenDBRequest });
    G.IDBVersionChangeEvent = class IDBVersionChangeEvent extends Event { constructor(t, o = {}) { super(t); this.oldVersion = o.oldVersion ?? 0; this.newVersion = o.newVersion ?? null; } };
  }
  G.atob = G.atob || (s => { const t = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'; let o = '', b = 0, n = 0; for (const c of String(s).replace(/[^A-Za-z0-9+/]/g, '')) { b = (b << 6) | t.indexOf(c); n += 6; if (n >= 8) { n -= 8; o += String.fromCharCode((b >> n) & 255); } } return o; });
  G.btoa = G.btoa || (s => { const t = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'; let o = ''; s = String(s); for (let i = 0; i < s.length; i += 3) { const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2); o += t[a >> 2] + t[((a & 3) << 4) | (b >> 4 || 0)] + (isNaN(b) ? '=' : t[((b & 15) << 2) | (c >> 6 || 0)]) + (isNaN(c) ? '=' : t[c & 63]); } return o; });

  // ----- bytes, blobs, components: the parts bundles take for granted -----
  G.TextEncoder = G.TextEncoder || class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(str = '') {
      const s = unescape(encodeURIComponent(String(str)));
      const out = new Uint8Array(s.length);
      for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
      return out;
    }
    encodeInto(str, dest) { const b = this.encode(str); const n = Math.min(b.length, dest.length); dest.set(b.subarray(0, n)); return { read: str.length, written: n }; }
  };
  G.TextDecoder = G.TextDecoder || class TextDecoder {
    constructor(label = 'utf-8') { this.encoding = String(label).toLowerCase(); }
    decode(buf) {
      if (!buf) return '';
      const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf.buffer || buf, buf.byteOffset || 0, buf.byteLength ?? buf.length);
      let s = '';
      for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 8192));
      if (this.encoding !== 'utf-8' && this.encoding !== 'utf8') return s;
      try { return decodeURIComponent(escape(s)); } catch (e) { return s; }
    }
  };
  G.Blob = class Blob {
    constructor(parts = [], opts = {}) {
      this.type = String(opts.type || '').toLowerCase();
      this.__s = parts.map(p => typeof p === 'string' ? p : p instanceof Blob ? p.__s : p && p.byteLength !== undefined ? new G.TextDecoder().decode(p) : String(p)).join('');
    }
    get size() { return new G.TextEncoder().encode(this.__s).length; }
    text() { return Promise.resolve(this.__s); }
    arrayBuffer() { return Promise.resolve(new G.TextEncoder().encode(this.__s).buffer); }
    slice(a, b, type) { return new Blob([this.__s.slice(a, b)], { type }); }
  };
  G.File = class File extends G.Blob { constructor(parts, name, opts = {}) { super(parts, opts); this.name = String(name); this.lastModified = 0; } };
  G.FileReader = class FileReader extends EventTarget {
    __done(v) { this.result = v; this.readyState = 2; setTimeout(() => { if (this.onload) this.onload({ target: this }); this.dispatchEvent(new Event('load')); if (this.onloadend) this.onloadend({ target: this }); }, 0); }
    readAsText(b) { this.__done(b.__s); }
    readAsDataURL(b) { this.__done('data:' + (b.type || 'application/octet-stream') + ';base64,' + btoa(unescape(encodeURIComponent(b.__s)))); }
    readAsArrayBuffer(b) { this.__done(new G.TextEncoder().encode(b.__s).buffer); }
  };
  // object URLs point nowhere: there is no blob store
  URL.createObjectURL = () => 'blob:null/0';
  URL.revokeObjectURL = () => {};
  G.customElements = {
    define(name, cls) {
      name = String(name).toLowerCase();
      if (defined.has(name)) throw new DOMException(`${name} is already defined`, 'NotSupportedError');
      defined.set(name, cls);
      names.set(cls, name);
      // elements already in the document become instances now
      for (const id of __dom_query(0, name, true)) {
        let el = wrappers.get(id);
        if (el && !(el instanceof cls)) {
          upgrading = id;
          try { el = new cls(); } catch (e) { log('custom element', name, String(e && e.message), String(e && e.stack)); } finally { upgrading = undefined; }
        } else if (!el) el = wrap(id);
        for (const a of cls.observedAttributes || []) if (el.hasAttribute(a) && el.attributeChangedCallback) { try { el.attributeChangedCallback(a, null, el.getAttribute(a)); } catch (e) { log('custom element', name, String(e)); } }
        connectAll(id, true);
      }
      const waiting = whens.get(name);
      if (waiting) { whens.delete(name); waiting(cls); }
    },
    get: name => defined.get(String(name).toLowerCase()),
    getName: cls => names.get(cls) || null,
    whenDefined(name) {
      name = String(name).toLowerCase();
      if (defined.has(name)) return Promise.resolve(defined.get(name));
      return new Promise(r => { const prev = whens.get(name); whens.set(name, cls => { if (prev) prev(cls); r(cls); }); });
    },
    upgrade() {},
  };
  const whens = new Map();
  // no workers: a page gets told at construction and falls back
  G.Worker = G.SharedWorker = class Worker { constructor() { throw new Error('workers are not supported'); } };
  // CacheStorage, in memory for the page's life (as a private window keeps it)
  {
    const keyOf = r => (typeof r === 'string' ? new URL(r, location.href).href : r && r.url) || String(r);
    class Cache {
      constructor() { this.__m = new Map(); }
      async match(r) { const v = this.__m.get(keyOf(r)); return v ? v.clone() : undefined; }
      async matchAll(r) { return r === undefined ? [...this.__m.values()].map(v => v.clone()) : [await this.match(r)].filter(Boolean); }
      async put(r, res) { this.__m.set(keyOf(r), res.clone ? res.clone() : res); }
      async add(r) { const res = await fetch(r); if (!res.ok) throw new TypeError('Request failed'); await this.put(r, res); }
      async addAll(rs) { for (const r of rs) await this.add(r); }
      async delete(r) { return this.__m.delete(keyOf(r)); }
      async keys() { return [...this.__m.keys()].map(u => new Request(u)); }
    }
    class CacheStorage {
      constructor() { this.__c = new Map(); }
      async open(n) { n = String(n); if (!this.__c.has(n)) this.__c.set(n, new Cache()); return this.__c.get(n); }
      async has(n) { return this.__c.has(String(n)); }
      async delete(n) { return this.__c.delete(String(n)); }
      async keys() { return [...this.__c.keys()]; }
      async match(r) { for (const c of this.__c.values()) { const v = await c.match(r); if (v) return v; } return undefined; }
    }
    G.Cache = Cache; G.CacheStorage = CacheStorage;
    G.caches = new CacheStorage();
  }
  G.BroadcastChannel = class BroadcastChannel extends EventTarget { constructor(name) { super(); this.name = name; } postMessage() {} close() {} };
  G.MessageChannel = class MessageChannel { constructor() { const a = new EventTarget(), b = new EventTarget(); a.postMessage = d => setTimeout(() => { b.dispatchEvent(Object.assign(new Event('message'), { data: d })); }, 0); b.postMessage = d => setTimeout(() => { a.dispatchEvent(Object.assign(new Event('message'), { data: d })); }, 0); a.start = b.start = a.close = b.close = () => {}; this.port1 = a; this.port2 = b; } };
  G.postMessage = (data) => setTimeout(() => { const e = new Event('message'); e.data = data; e.origin = location.origin; e.source = G; G.dispatchEvent(e); }, 0);
  G.WeakRef = G.WeakRef || class WeakRef { constructor(t) { this.t = t; } deref() { return this.t; } };
  G.FinalizationRegistry = G.FinalizationRegistry || class FinalizationRegistry { register() {} unregister() {} };
  G.scheduler = {
    postTask: fn => new Promise((resolve, reject) => setTimeout(() => {
      try { resolve(fn()); } catch (error) { reject(error); }
    }, 0)),
    yield: () => new Promise(resolve => setTimeout(resolve, 0)),
  };


  // ----- walking the tree --------------------------------------------------
  G.NodeFilter = {
    FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3,
    SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 0x1, SHOW_ATTRIBUTE: 0x2, SHOW_TEXT: 0x4, SHOW_CDATA_SECTION: 0x8,
    SHOW_PROCESSING_INSTRUCTION: 0x40, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_TYPE: 0x200, SHOW_DOCUMENT_FRAGMENT: 0x400,
  };
  const accepts = (walker, node) => {
    if (!(walker.whatToShow & (1 << (node.nodeType - 1)))) return 3;
    const f = walker.filter;
    if (!f) return 1;
    return typeof f === 'function' ? f(node) : f.acceptNode(node);
  };
  G.TreeWalker = class TreeWalker {
    constructor(root, whatToShow = 0xFFFFFFFF, filter = null) { this.root = root; this.whatToShow = whatToShow >>> 0; this.filter = filter; this.currentNode = root; }
    __step(next) {
      // document order (or its reverse), staying under root, skipping rejected subtrees
      let n = this.currentNode;
      for (;;) {
        n = next(n);
        if (!n) return null;
        const r = accepts(this, n);
        if (r === 1) { this.currentNode = n; return n; }
      }
    }
    nextNode() {
      const root = this.root;
      const following = n => {
        if (n.firstChild && accepts(this, n) !== 2) return n.firstChild;
        for (let m = n; m && m !== root; m = m.parentNode) if (m.nextSibling) return m.nextSibling;
        return null;
      };
      return this.__step(following);
    }
    previousNode() {
      const root = this.root;
      const preceding = n => {
        if (n === root) return null;
        let m = n.previousSibling;
        if (!m) return n.parentNode === root ? (accepts(this, root) === 1 ? null : null) : n.parentNode;
        while (m.lastChild && accepts(this, m) !== 2) m = m.lastChild;
        return m;
      };
      return this.__step(preceding);
    }
    parentNode() { for (let n = this.currentNode; n && n !== this.root;) { n = n.parentNode; if (n && accepts(this, n) === 1) { this.currentNode = n; return n; } } return null; }
    firstChild() { for (let c = this.currentNode.firstChild; c; c = c.nextSibling) if (accepts(this, c) === 1) { this.currentNode = c; return c; } return null; }
    lastChild() { for (let c = this.currentNode.lastChild; c; c = c.previousSibling) if (accepts(this, c) === 1) { this.currentNode = c; return c; } return null; }
    nextSibling() { for (let c = this.currentNode.nextSibling; c; c = c.nextSibling) if (accepts(this, c) === 1) { this.currentNode = c; return c; } return null; }
    previousSibling() { for (let c = this.currentNode.previousSibling; c; c = c.previousSibling) if (accepts(this, c) === 1) { this.currentNode = c; return c; } return null; }
  };
  G.NodeIterator = class NodeIterator extends G.TreeWalker {
    get referenceNode() { return this.currentNode; }
    nextNode() { if (!this.__started) { this.__started = true; if (accepts(this, this.root) === 1) return this.root; } return super.nextNode(); }
    detach() {}
  };
  Document.prototype.createTreeWalker = (root, what, filter) => new G.TreeWalker(root, what, filter);
  Document.prototype.createNodeIterator = (root, what, filter) => new G.NodeIterator(root, what, filter);


  // ----- geometry ---------------------------------------------------------
  G.DOMRectReadOnly = class DOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) { this.x = +x; this.y = +y; this.width = +width; this.height = +height; }
    get left() { return Math.min(this.x, this.x + this.width); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    static fromRect(r = {}) { return new this(r.x, r.y, r.width, r.height); }
    toJSON() { const { x, y, width, height, top, right, bottom, left } = this; return { x, y, width, height, top, right, bottom, left }; }
  };
  G.DOMRect = class DOMRect extends G.DOMRectReadOnly {};
  G.DOMPoint = G.DOMPointReadOnly = class DOMPoint { constructor(x = 0, y = 0, z = 0, w = 1) { Object.assign(this, { x, y, z, w }); } static fromPoint(p = {}) { return new this(p.x, p.y, p.z, p.w); } };
  G.DOMMatrix = G.DOMMatrixReadOnly = class DOMMatrix { constructor() { Object.assign(this, { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, m11: 1, m12: 0, m21: 0, m22: 1, m41: 0, m42: 0, is2D: true, isIdentity: true }); } };

  // ----- ranges and the selection --------------------------------------------
  {
    const indexIn = n => { const p = n.parentNode; return p ? p.childNodes.indexOf(n) : 0; };
    const lengthOf = n => n.nodeType === 3 || n.nodeType === 8 ? n.data.length : n.childNodes.length;
    // document order of two boundary points: -1, 0, 1
    const comparePoints = (a, ao, b, bo) => {
      if (a === b) return ao === bo ? 0 : ao < bo ? -1 : 1;
      const chain = n => { const out = []; for (let x = n; x; x = x.parentNode) out.unshift(x); return out; };
      const ca = chain(a), cb = chain(b);
      let i = 0; while (i < ca.length && i < cb.length && ca[i] === cb[i]) i++;
      if (i === ca.length) { const child = cb[i]; return indexIn(child) < ao ? 1 : -1; }      // a contains b
      if (i === cb.length) { const child = ca[i]; return indexIn(child) < bo ? -1 : 1; }      // b contains a
      return indexIn(ca[i]) < indexIn(cb[i]) ? -1 : 1;
    };
    G.Range = class Range {
      constructor() { this.startContainer = this.endContainer = document; this.startOffset = this.endOffset = 0; }
      get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
      get commonAncestorContainer() { let a = this.startContainer; while (a && !(a === this.endContainer || a.contains(this.endContainer))) a = a.parentNode; return a || document; }
      setStart(n, o) { this.startContainer = n; this.startOffset = o; if (comparePoints(n, o, this.endContainer, this.endOffset) > 0 || this.endContainer.getRootNode?.() !== n.getRootNode?.()) this.setEnd(n, o); }
      setEnd(n, o) { this.endContainer = n; this.endOffset = o; if (comparePoints(this.startContainer, this.startOffset, n, o) > 0) { this.startContainer = n; this.startOffset = o; } }
      setStartBefore(n) { this.setStart(n.parentNode, indexIn(n)); }
      setStartAfter(n) { this.setStart(n.parentNode, indexIn(n) + 1); }
      setEndBefore(n) { this.setEnd(n.parentNode, indexIn(n)); }
      setEndAfter(n) { this.setEnd(n.parentNode, indexIn(n) + 1); }
      collapse(toStart) { if (toStart) { this.endContainer = this.startContainer; this.endOffset = this.startOffset; } else { this.startContainer = this.endContainer; this.startOffset = this.endOffset; } }
      selectNode(n) { this.setStartBefore(n); this.setEndAfter(n); }
      selectNodeContents(n) { this.startContainer = this.endContainer = n; this.startOffset = 0; this.endOffset = lengthOf(n); }
      cloneRange() { const r = new Range(); Object.assign(r, { startContainer: this.startContainer, startOffset: this.startOffset, endContainer: this.endContainer, endOffset: this.endOffset }); return r; }
      compareBoundaryPoints(how, other) {
        const [a, ao] = how === 0 || how === 3 ? [this.startContainer, this.startOffset] : [this.endContainer, this.endOffset];
        const [b, bo] = how === 0 || how === 1 ? [other.startContainer, other.startOffset] : [other.endContainer, other.endOffset];
        return comparePoints(a, ao, b, bo);
      }
      comparePoint(n, o) { if (comparePoints(n, o, this.startContainer, this.startOffset) < 0) return -1; if (comparePoints(n, o, this.endContainer, this.endOffset) > 0) return 1; return 0; }
      isPointInRange(n, o) { return this.comparePoint(n, o) === 0; }
      intersectsNode(n) { const p = n.parentNode; if (!p) return true; const i = indexIn(n); return comparePoints(p, i, this.endContainer, this.endOffset) < 0 && comparePoints(p, i + 1, this.startContainer, this.startOffset) > 0; }
      // the nodes fully or partly inside, split at text boundaries
      __walk(fn) {
        const { startContainer: sc, startOffset: so, endContainer: ec, endOffset: eo } = this;
        if (sc === ec && (sc.nodeType === 3 || sc.nodeType === 8)) return fn('text', sc, so, eo);
        const root = this.commonAncestorContainer;
        const all = [];
        const walk = n => { for (const c of n.childNodes) { all.push(c); walk(c); } };
        walk(root);
        for (const n of all) {
          const p = n.parentNode, i = indexIn(n);
          const startsAfter = comparePoints(p, i, sc, so) >= 0 || (n === sc);
          const endsBefore = comparePoints(p, i + 1, ec, eo) <= 0 || (n === ec);
          if (n === sc && (n.nodeType === 3)) fn('text', n, so, n.data.length);
          else if (n === ec && (n.nodeType === 3)) fn('text', n, 0, eo);
          else if (startsAfter && endsBefore && !all.includes(p) || (startsAfter && endsBefore && p === root)) fn('node', n);
        }
      }
      toString() { let out = ''; this.__walk((kind, n, a, b) => { out += kind === 'text' ? n.data.slice(a, b) : n.textContent; }); return out; }
      cloneContents() { const f = document.createDocumentFragment(); this.__walk((kind, n, a, b) => f.append(kind === 'text' ? document.createTextNode(n.data.slice(a, b)) : n.cloneNode(true))); return f; }
      deleteContents() {
        const parts = []; this.__walk((...x) => parts.push(x));
        for (const [kind, n, a, b] of parts) { if (kind === 'text') n.data = n.data.slice(0, a) + n.data.slice(b); else n.remove(); }
        this.collapse(true);
      }
      extractContents() { const f = this.cloneContents(); this.deleteContents(); return f; }
      insertNode(node) {
        const { startContainer: c, startOffset: o } = this;
        if (c.nodeType === 3) { const rest = c.splitText(o); c.parentNode.insertBefore(node, rest); }
        else c.insertBefore(node, c.childNodes[o] || null);
      }
      surroundContents(el) { const f = this.extractContents(); el.append(f); this.insertNode(el); this.selectNode(el); }
      createContextualFragment(html) { const t = document.createElement('template'); t.innerHTML = String(html); return t.content; }
      getBoundingClientRect() { return new G.DOMRect(); }
      getClientRects() { return []; }
      detach() {}
    };
    Object.assign(G.Range, { START_TO_START: 0, START_TO_END: 1, END_TO_END: 2, END_TO_START: 3 });
    G.StaticRange = class StaticRange { constructor(i) { Object.assign(this, { startContainer: i.startContainer, startOffset: i.startOffset, endContainer: i.endContainer, endOffset: i.endOffset }); } get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; } };
    Document.prototype.createRange = () => new G.Range();
    G.Selection = class Selection {
      constructor() { this.__ranges = []; }
      get rangeCount() { return this.__ranges.length; }
      get isCollapsed() { return !this.__ranges.length || this.__ranges[0].collapsed; }
      get anchorNode() { return this.__ranges[0] ? this.__ranges[0].startContainer : null; }
      get anchorOffset() { return this.__ranges[0] ? this.__ranges[0].startOffset : 0; }
      get focusNode() { return this.__ranges[0] ? this.__ranges[0].endContainer : null; }
      get focusOffset() { return this.__ranges[0] ? this.__ranges[0].endOffset : 0; }
      get type() { return !this.__ranges.length ? 'None' : this.isCollapsed ? 'Caret' : 'Range'; }
      getRangeAt(i) { if (!this.__ranges[i]) throw new DOMException('Index out of range', 'IndexSizeError'); return this.__ranges[i]; }
      addRange(r) { this.__ranges = [r]; }
      removeRange() { this.__ranges = []; }
      removeAllRanges() { this.__ranges = []; }
      empty() { this.__ranges = []; }
      collapse(n, o = 0) { if (!n) { this.__ranges = []; return; } const r = new G.Range(); r.setStart(n, o); this.__ranges = [r]; }
      setPosition(n, o) { this.collapse(n, o); }
      collapseToStart() { if (this.__ranges[0]) this.__ranges[0].collapse(true); }
      collapseToEnd() { if (this.__ranges[0]) this.__ranges[0].collapse(false); }
      extend(n, o = 0) { if (this.__ranges[0]) this.__ranges[0].setEnd(n, o); }
      setBaseAndExtent(a, ao, f, fo) { const r = new G.Range(); r.setStart(a, ao); r.setEnd(f, fo); this.__ranges = [r]; }
      selectAllChildren(n) { const r = new G.Range(); r.selectNodeContents(n); this.__ranges = [r]; }
      containsNode(n, partly) { return this.__ranges.some(r => partly ? r.intersectsNode(n) : r.intersectsNode(n)); }
      deleteFromDocument() { if (this.__ranges[0]) this.__ranges[0].deleteContents(); }
      modify() {}
      toString() { return this.__ranges.map(String).join(''); }
    };
    const selection = new G.Selection();
    G.getSelection = () => selection;
    Document.prototype.getSelection = () => selection;
  }

  // ----- parsing markup into a document of its own -------------------------
  class ParsedDocument extends DocumentFragment {
    get documentElement() { return this.firstChild; }
    get head() { return this.documentElement.querySelector('head'); }
    get body() { return this.documentElement.querySelector('body'); }
    get title() { const t = this.querySelector('title'); return t ? t.textContent : ''; }
    get nodeType() { return 9; }
    getElementById(id) { return this.querySelector('#' + CSS.escape(String(id))); }
    getElementsByTagName(t) { return this.querySelectorAll(t); }
    getElementsByClassName(c) { return this.querySelectorAll(String(c).trim().split(/\s+/).map(x => '.' + CSS.escape(x)).join('')); }
    createElement(t) { return document.createElement(t); }
    createTextNode(t) { return document.createTextNode(t); }
    importNode(n, deep) { return n.cloneNode(deep); }
    adoptNode(n) { return n; }
  }
  const headTags = new Set(['title', 'meta', 'link', 'style', 'base', 'script', 'noscript', 'template']);
  G.DOMParser = class DOMParser {
    parseFromString(markup, type = 'text/html') {
      const doc = new ParsedDocument(__dom_create('#fragment'));
      const html = document.createElement('html'), head = document.createElement('head'), body = document.createElement('body');
      html.append(head, body);
      doc.append(html);
      const box = document.createElement('div');
      // the fragment parser drops <html>/<head>/<body> tags themselves, keeping what is in them
      box.innerHTML = String(markup);
      let inBody = false;
      for (const n of box.childNodes) {
        if (!inBody && n.nodeType === 1 && headTags.has(n.localName)) head.append(n);
        else if (!inBody && n.nodeType === 3 && !n.textContent.trim()) continue;
        else { inBody = true; body.append(n); }
      }
      return doc;
    }
  };
  Object.defineProperty(Document.prototype, 'implementation', {
    get() {
      return {
        createHTMLDocument(title) {
          const doc = new G.DOMParser().parseFromString('', 'text/html');
          if (title !== undefined) { const t = document.createElement('title'); t.textContent = String(title); doc.head.append(t); }
          return doc;
        },
        createDocument() { return new G.DOMParser().parseFromString('', 'text/html'); },
        createDocumentType(name, publicId, systemId) { return { name, publicId, systemId, nodeType: 10 }; },
        hasFeature: () => true,
      };
    },
    configurable: true,
  });
  Object.assign(ParsedDocument.prototype, {
    createTreeWalker: (root, what, filter) => new G.TreeWalker(root, what, filter),
    createNodeIterator: (root, what, filter) => new G.NodeIterator(root, what, filter),
    createDocumentFragment: () => document.createDocumentFragment(),
    createComment: t => document.createComment(t),
    createEvent: t => document.createEvent(t),
    get implementation() { return document.implementation; },
    get defaultView() { return null; },
  });
  G.XMLSerializer = class XMLSerializer { serializeToString(n) { return n.nodeType === 9 && n.documentElement ? n.documentElement.outerHTML : n.outerHTML ?? n.textContent ?? ''; } };

  // ----- streams (bodies arrive whole; a stream hands them over in one chunk)
  G.ReadableStream = class ReadableStream {
    constructor(source = {}) {
      this.__queue = []; this.__closed = false; this.__error = null; this.__waiting = [];
      const wake = () => { while (this.__waiting.length && (this.__queue.length || this.__closed || this.__error)) { const w = this.__waiting.shift(); if (this.__error) w.reject(this.__error); else if (this.__queue.length) w.resolve({ value: this.__queue.shift(), done: false }); else w.resolve({ value: undefined, done: true }); } };
      this.__controller = { enqueue: c => { this.__queue.push(c); wake(); }, close: () => { this.__closed = true; wake(); }, error: e => { this.__error = e; wake(); }, desiredSize: 1 };
      this.__pull = source.pull;
      try { const r = source.start && source.start(this.__controller); if (r && r.then) r.catch(e => this.__controller.error(e)); } catch (e) { this.__error = e; }
    }
    get locked() { return !!this.__reader; }
    getReader() {
      const stream = this;
      return this.__reader = {
        read() {
          if (stream.__queue.length) return Promise.resolve({ value: stream.__queue.shift(), done: false });
          if (stream.__error) return Promise.reject(stream.__error);
          if (stream.__closed) return Promise.resolve({ value: undefined, done: true });
          if (stream.__pull) { try { stream.__pull(stream.__controller); } catch (e) { return Promise.reject(e); } }
          return new Promise((resolve, reject) => { stream.__waiting.push({ resolve, reject }); });
        },
        releaseLock() { stream.__reader = null; },
        cancel() { stream.__closed = true; return Promise.resolve(); },
        get closed() { return Promise.resolve(); },
      };
    }
    cancel() { this.__closed = true; return Promise.resolve(); }
    async *[Symbol.asyncIterator]() { const r = this.getReader(); for (;;) { const { value, done } = await r.read(); if (done) return; yield value; } }
    pipeThrough(t) { this.pipeTo(t.writable); return t.readable; }
    async pipeTo(w) { const wr = w.getWriter(); for await (const c of this) await wr.write(c); await wr.close(); }
    tee() { const parts = []; const r = this.getReader(); const mk = () => new ReadableStream({ start: c => parts.push(c) }); const a = mk(), b = mk(); (async () => { for (;;) { const { value, done } = await r.read(); for (const c of parts) done ? c.close() : c.enqueue(value); if (done) break; } })(); return [a, b]; }
  };
  G.WritableStream = class WritableStream {
    constructor(sink = {}) { this.__sink = sink; }
    getWriter() { const sink = this.__sink; return { write: c => Promise.resolve(sink.write && sink.write(c)), close: () => Promise.resolve(sink.close && sink.close()), abort: () => Promise.resolve(), releaseLock() {}, get ready() { return Promise.resolve(); }, get closed() { return Promise.resolve(); } }; }
  };
  G.TransformStream = class TransformStream {
    constructor(t = {}) {
      let out;
      this.readable = new G.ReadableStream({ start: c => { out = c; } });
      const controller = { enqueue: c => out.enqueue(c), terminate: () => out.close(), error: e => out.error(e) };
      if (t.start) t.start(controller);
      this.writable = new G.WritableStream({ write: c => t.transform ? t.transform(c, controller) : out.enqueue(c), close: () => { if (t.flush) t.flush(controller); out.close(); } });
    }
  };
  G.TextDecoderStream = class TextDecoderStream extends G.TransformStream { constructor() { const d = new G.TextDecoder(); super({ transform: (c, k) => k.enqueue(d.decode(c)) }); } };
  G.TextEncoderStream = class TextEncoderStream extends G.TransformStream { constructor() { const e = new G.TextEncoder(); super({ transform: (c, k) => k.enqueue(e.encode(c)) }); } };
  Object.defineProperty(Response.prototype, 'body', { get() { const bytes = new G.TextEncoder().encode(this.__body); return new G.ReadableStream({ start: c => { c.enqueue(bytes); c.close(); } }); }, configurable: true });
  Response.prototype.arrayBuffer = function () { return Promise.resolve(new G.TextEncoder().encode(this.__body).buffer); };
  Response.prototype.blob = function () { return Promise.resolve(new G.Blob([this.__body], { type: this.headers.get('content-type') || '' })); };
  Response.prototype.bodyUsed = false;
  // there is no canvas to draw on, but paths can still be built
  G.Path2D = class Path2D { constructor() {} addPath() {} closePath() {} moveTo() {} lineTo() {} bezierCurveTo() {} quadraticCurveTo() {} arc() {} arcTo() {} ellipse() {} rect() {} roundRect() {} };

  Object.assign(G, {
    window: G, self: G, top: G, parent: G, frames: G, globalThis: G,
    document, location, history,
    localStorage: new Storage(), sessionStorage: new Storage(),
    Event, CustomEvent, UIEvent, MouseEvent, PointerEvent, KeyboardEvent, InputEvent, FocusEvent, SubmitEvent, EventTarget, DOMException,
    Node: NodeBase, ShadowRoot, Element, HTMLElement, SVGElement, SVGGraphicsElement, SVGSVGElement, CharacterData, Text, Comment, Document, DocumentFragment, URL, URLSearchParams, Response, Headers, Storage,
    ...Object.fromEntries(Object.entries(classes).map(([name, cls]) => ['HTML' + name + 'Element', cls])),
  });
  // node interfaces this DOM never creates (HTML has no CDATA or PIs) but code tests for
  G.CDATASection = class CDATASection extends Text {};
  G.ProcessingInstruction = class ProcessingInstruction extends CharacterData {};
  G.DocumentType = class DocumentType extends NodeBase {};
  G.Attr = class Attr extends NodeBase {};
  // the rest of the event interfaces pages construct or test with instanceof
  eventClass('WheelEvent', MouseEvent, { deltaX: 0, deltaY: 0, deltaZ: 0, deltaMode: 0 });
  Object.assign(G.WheelEvent, { DOM_DELTA_PIXEL: 0, DOM_DELTA_LINE: 1, DOM_DELTA_PAGE: 2 });
  eventClass('DragEvent', MouseEvent, { dataTransfer: null });
  eventClass('TouchEvent', UIEvent, { touches: () => [], targetTouches: () => [], changedTouches: () => [], altKey: false, ctrlKey: false, metaKey: false, shiftKey: false });
  G.Touch = class Touch { constructor(i = {}) { Object.assign(this, { identifier: 0, target: null, clientX: 0, clientY: 0, pageX: 0, pageY: 0, screenX: 0, screenY: 0, radiusX: 0, radiusY: 0, force: 0 }, i); } };
  eventClass('CompositionEvent', UIEvent, { data: '' });
  eventClass('ClipboardEvent', Event, { clipboardData: null });
  eventClass('AnimationEvent', Event, { animationName: '', elapsedTime: 0, pseudoElement: '' });
  eventClass('TransitionEvent', Event, { propertyName: '', elapsedTime: 0, pseudoElement: '' });
  eventClass('ErrorEvent', Event, { message: '', filename: '', lineno: 0, colno: 0, error: null });
  eventClass('PromiseRejectionEvent', Event, { promise: null, reason: undefined });
  eventClass('HashChangeEvent', Event, { oldURL: '', newURL: '' });
  eventClass('PopStateEvent', Event, { state: null });
  eventClass('PageTransitionEvent', Event, { persisted: false });
  eventClass('MessageEvent', Event, { data: null, origin: '', lastEventId: '', source: null, ports: () => [] });
  eventClass('StorageEvent', Event, { key: null, oldValue: null, newValue: null, url: '', storageArea: null });
  eventClass('ProgressEvent', Event, { lengthComputable: false, loaded: 0, total: 0 });
  eventClass('CloseEvent', Event, { wasClean: false, code: 0, reason: '' });
  eventClass('BeforeUnloadEvent', Event, { returnValue: '' });
  eventClass('MediaQueryListEvent', Event, { media: '', matches: false });
  eventClass('SecurityPolicyViolationEvent', Event, { blockedURI: '', violatedDirective: '', effectiveDirective: '', originalPolicy: '', disposition: 'enforce', sample: '', statusCode: 0 });
  eventClass('FormDataEvent', Event, { formData: null });
  eventClass('ToggleEvent', Event, { oldState: '', newState: '' });
  eventClass('TrackEvent', Event, { track: null });
  // XPath: the constants pages read; evaluate() finds nothing (no XPath engine yet)
  G.XPathResult = class XPathResult {
    constructor(type) { this.resultType = type; this.snapshotLength = 0; this.singleNodeValue = null; this.booleanValue = false; this.numberValue = NaN; this.stringValue = ''; this.invalidIteratorState = false; }
    iterateNext() { return null; } snapshotItem() { return null; }
  };
  Object.assign(G.XPathResult, { ANY_TYPE: 0, NUMBER_TYPE: 1, STRING_TYPE: 2, BOOLEAN_TYPE: 3, UNORDERED_NODE_ITERATOR_TYPE: 4, ORDERED_NODE_ITERATOR_TYPE: 5, UNORDERED_NODE_SNAPSHOT_TYPE: 6, ORDERED_NODE_SNAPSHOT_TYPE: 7, ANY_UNORDERED_NODE_TYPE: 8, FIRST_ORDERED_NODE_TYPE: 9 });
  Document.prototype.evaluate = (expr, ctx, ns, type) => new G.XPathResult(type || 0);
  Document.prototype.createExpression = () => ({ evaluate: (ctx, type) => new G.XPathResult(type || 0) });
  Document.prototype.createNSResolver = n => n;
  // the window's own interface, for code that patches Window.prototype
  G.Window = function Window() { throw new TypeError('Illegal constructor'); };
  G.Window.prototype = Object.create(EventTarget.prototype, { constructor: { value: G.Window, writable: true, configurable: true } });
  // window → Window.prototype → EventTarget.prototype, as in a browser
  try { Object.setPrototypeOf(G, G.Window.prototype); } catch (e) { log('window prototype', String(e)); }
  G.Image = function Image(w, h) { const img = document.createElement('img'); if (w) img.setAttribute('width', w); if (h) img.setAttribute('height', h); return img; };

  // ----- what the window calls ---------------------------------------------
  // which <script> is running: bundles find their own URL through it
  const ran = new Set();
  G.__sepia_current = (src, head) => {
    const abs = v => { try { return new URL(v, location.href).href; } catch (e) { return v; } };
    if (src) {
      for (const s of document.querySelectorAll('script[src]')) if (abs(s.getAttribute('src')) === src) { G.__currentScript = s; return; }
      const s = document.createElement('script'); s.setAttribute('src', src); G.__currentScript = s; return;
    }
    G.__currentScript = null;
    for (const s of document.querySelectorAll('script:not([src])')) {
      if (!ran.has(s) && s.textContent.startsWith(head)) { ran.add(s); G.__currentScript = s; return; }
    }
  };
  G.__sepia_loaded = () => {
    G.__currentScript = null;
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
    const click = new MouseEvent('click', { bubbles: true, cancelable: true });
    const go = el.dispatchEvent(click);
    // inline handler attributes (`<button onclick="...">`) never go through
    // addEventListener, so run them here the way a browser would
    let inline = true;
    const handler = el.getAttribute && el.getAttribute('onclick');
    if (handler) {
      try {
        inline = Function('event', handler).call(el, click) !== false;
      } catch (err) {
        inline = true;
      }
    }
    if (!go || !inline) return;
    // default actions
    const button = el.closest('button, input[type=submit]');
    if (button && button.disabled) return;
    const form = button && button.form;
    if (form && isSubmitButton(button)) {
      form.requestSubmit(button);
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
  function isSubmitButton(el) {
    const type = (el.getAttribute('type') || (el.localName === 'button' ? 'submit' : 'text')).toLowerCase();
    return (el.localName === 'button' || el.localName === 'input') && type === 'submit';
  }
  function submitForm(form, submitter = null) {
    const method = ((submitter && submitter.getAttribute('formmethod')) || form.getAttribute('method') || 'get').toLowerCase();
    const action = (submitter && submitter.getAttribute('formaction')) || form.getAttribute('action') || location.href;
    const body = new URLSearchParams(formPairs(form, submitter)).toString();
    if (method === 'dialog') throw new TypeError('Dialog form submission is not supported');
    if (method === 'post') __page_navigate(action, body);
    else {
      // Replace the query while preserving the action's fragment.
      const hashAt = action.indexOf('#');
      const base = hashAt < 0 ? action : action.slice(0, hashAt);
      const hash = hashAt < 0 ? '' : action.slice(hashAt);
      __page_navigate(base.split('?')[0] + '?' + body + hash);
    }
  }
  function formPairs(form, submitter = null) {
    const pairs = [];
    for (const f of form.querySelectorAll('input, select, textarea, button')) {
      if (!f.name || f.disabled) continue;
      const type = f.type.toLowerCase();
      if ((type === 'checkbox' || type === 'radio') && !f.checked) continue;
      if (f.localName === 'button' || ['submit', 'button', 'reset', 'file', 'image'].includes(type)) {
        if (f !== submitter || !isSubmitButton(f)) continue;
      }
      if (f.localName === 'select') {
        const opts = f.querySelectorAll('option');
        const selected = opts.filter(o => o.selected);
        const chosen = f.multiple ? selected : selected.length ? [selected[0]] : opts.slice(0, 1);
        for (const o of chosen) if (!o.disabled && !(o.parentElement && o.parentElement.localName === 'optgroup' && o.parentElement.disabled)) pairs.push([f.name, f.multiple ? (o.getAttribute('value') ?? o.textContent) : f.value]);
      } else {
        const value = (type === 'checkbox' || type === 'radio') && !f.hasAttribute('value') ? 'on' : f.value;
        pairs.push([f.name, value]);
      }
    }
    return pairs;
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
      form.requestSubmit();
      return;
    }
    if (down && key === 'Enter' && el.dispatchEvent) el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  G.__sepia_resize = (width, height) => {
    const changed = G.innerWidth !== width || G.innerHeight !== height;
    G.innerWidth = G.outerWidth = width;
    G.innerHeight = G.outerHeight = height;
    if (changed) G.dispatchEvent(new Event('resize'));
  };
  G.__sepia_hash = hash => { location.hash = hash; };
  // a rejected promise nobody handled (js.rs reports them after the jobs ran)
  G.__sepia_unhandled = (promise, reason) => {
    const e = new G.PromiseRejectionEvent('unhandledrejection', { cancelable: true, promise, reason });
    if (G.dispatchEvent(e)) log('Uncaught (in promise)', reason);
  };

  // everything above is the browser: its functions print as native code, as
  // in any browser (pages test `fetch.toString()` for "[native code]")
  {
    const builtin = new WeakMap();   // function -> the name a browser prints
    const seen = new WeakSet();
    const mark = (obj, depth) => {
      if (!obj || (typeof obj !== 'object' && typeof obj !== 'function') || seen.has(obj) || depth > 3) return;
      seen.add(obj);
      for (const key of Reflect.ownKeys(obj)) {
        let d;
        try { d = Object.getOwnPropertyDescriptor(obj, key); } catch (e) { continue; }
        if (!d) continue;
        const keyName = typeof key === 'symbol' ? '' : String(key);
        for (const f of [d.value, d.get, d.set]) {
          if (typeof f === 'function') { if (!builtin.has(f)) builtin.set(f, keyName); if (f.prototype) mark(f.prototype, depth + 1); mark(f, depth + 1); }
          else if (f && typeof f === 'object' && depth < 2 && !(f instanceof NodeBase)) mark(f, depth + 1);
        }
      }
    };
    mark(G, 0);
    const toString = Function.prototype.toString;
    const nativeToString = { toString() {
      if (builtin.has(this)) {
        const own = typeof this.name === 'string' ? this.name.replace(/^(get|set|bound) /, '') : '';
        const name = own || builtin.get(this) || '';
        return `function ${name}() {\n    [native code]\n}`;
      }
      return toString.call(this);
    } }.toString;
    Object.defineProperty(Function.prototype, 'toString', { value: nativeToString, writable: true, configurable: true });
    builtin.set(nativeToString, 'toString');
  }
})();
