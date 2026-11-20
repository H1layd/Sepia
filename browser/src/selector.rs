//! CSS selectors by the `selectors` crate (Servo / Firefox): parsing and
//! matching of Selectors Level 4 — :is() :where() :not() :has() with full
//! selector lists, :nth-child(An+B of S), attribute flags, specificity —
//! over this engine's tree.
//!
//! The tree has no parent pointers: callers pass each element's ancestors
//! (outermost first), as layout always has them. Before matching, that
//! chain is entered in a per-thread parent table; siblings and children
//! reached while matching (`~`, `+`, `:has()`) are entered as they are
//! reached. An element is then just a reference to its node.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use cssparser::ToCss;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::{MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode, SelectorCaches};
use selectors::matching::ElementSelectorFlags;
use selectors::parser::{ParseRelative, SelectorParseErrorKind};
use selectors::{OpaqueElement, SelectorImpl};

use crate::dom::Node;

// ---------------------------------------------------------------------------
// the implementation's types
// ---------------------------------------------------------------------------

/// A string with its hash ready (identifiers, names, attribute values).
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct CssStr(pub String, u32);

impl CssStr {
    pub fn new(s: &str) -> CssStr {
        let mut h: u32 = 0x811c9dc5;
        for b in s.bytes() {
            h = (h ^ b as u32).wrapping_mul(0x01000193);
        }
        CssStr(s.to_string(), h)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'a> From<&'a str> for CssStr {
    fn from(s: &'a str) -> CssStr {
        CssStr::new(s)
    }
}

impl AsRef<str> for CssStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl precomputed_hash::PrecomputedHash for CssStr {
    fn precomputed_hash(&self) -> u32 {
        self.1
    }
}

impl ToCss for CssStr {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_identifier(&self.0, dest)
    }
}

/// Non-structural pseudo-classes this engine evaluates on a static page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PseudoClass {
    Link,
    AnyLink,
    Visited,
    /// interaction states nothing is in on a page nobody touched
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Target,
    Checked,
    Indeterminate,
    Default,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Defined,
    Open,
    Valid,
    Invalid,
    Lang(String),
    Dir(String),
    /// what a static render is never in (:fullscreen, :autofill, :modal…)
    Never(String),
}

impl selectors::parser::NonTSPseudoClass for PseudoClass {
    fn is_active_or_hover(&self) -> bool {
        matches!(self, PseudoClass::Hover | PseudoClass::Active)
    }

    fn is_user_action_state(&self) -> bool {
        matches!(self, PseudoClass::Hover | PseudoClass::Active | PseudoClass::Focus | PseudoClass::FocusVisible | PseudoClass::FocusWithin)
    }
}

impl ToCss for PseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        match self {
            PseudoClass::Lang(l) => write!(dest, ":lang({l})"),
            PseudoClass::Dir(d) => write!(dest, ":dir({d})"),
            PseudoClass::Never(n) => write!(dest, ":{n}"),
            other => write!(dest, ":{}", format!("{other:?}").to_ascii_lowercase()),
        }
    }
}

/// Pseudo-elements: ::before / ::after make boxes; the others style parts
/// this engine does not draw (their rules never apply to an element).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PseudoElement {
    Before,
    After,
    Other(String),
}

impl selectors::parser::PseudoElement for PseudoElement {
    fn is_before_or_after(&self) -> bool {
        matches!(self, PseudoElement::Before | PseudoElement::After)
    }

    fn accepts_state_pseudo_classes(&self) -> bool {
        true
    }

    fn valid_after_slotted(&self) -> bool {
        true
    }
}

impl ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        match self {
            PseudoElement::Before => dest.write_str("::before"),
            PseudoElement::After => dest.write_str("::after"),
            PseudoElement::Other(n) => write!(dest, "::{n}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Simple;

impl SelectorImpl for Simple {
    type ExtraMatchingData<'a> = std::marker::PhantomData<&'a ()>;
    type AttrValue = CssStr;
    type Identifier = CssStr;
    type LocalName = CssStr;
    type NamespaceUrl = CssStr;
    type NamespacePrefix = CssStr;
    type BorrowedLocalName = CssStr;
    type BorrowedNamespaceUrl = CssStr;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoElement;
}

// ---------------------------------------------------------------------------
// parsing
// ---------------------------------------------------------------------------

struct SelectorParser;

fn unsupported() -> cssparser::ParseError<SelectorParseErrorKind> {
    cssparser::ParseError::custom(SelectorParseErrorKind::UnsupportedPseudoClassOrElement)
}

impl<'i> selectors::parser::Parser<'i> for SelectorParser {
    type Impl = Simple;
    type Error = SelectorParseErrorKind;

    fn parse_nth_child_of(&self) -> bool {
        true
    }
    fn parse_is_and_where(&self) -> bool {
        true
    }
    fn parse_has(&self) -> bool {
        true
    }
    fn parse_parent_selector(&self) -> bool {
        true
    }
    // valid in any stylesheet, though only a shadow tree's elements match:
    // a list like Tailwind's `:root, :host` must not be dropped for them
    fn parse_host(&self) -> bool {
        true
    }
    fn parse_slotted(&self) -> bool {
        true
    }
    fn parse_part(&self) -> bool {
        true
    }
    fn is_is_alias(&self, name: &str) -> bool {
        // Chrome still reads the old :-webkit-any() as :is()
        name.eq_ignore_ascii_case("-webkit-any") || name.eq_ignore_ascii_case("matches")
    }

    fn parse_non_ts_pseudo_class(&self, name: &str) -> Result<PseudoClass, cssparser::ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "link" => PseudoClass::Link,
            "any-link" | "-webkit-any-link" => PseudoClass::AnyLink,
            "visited" => PseudoClass::Visited,
            "hover" => PseudoClass::Hover,
            "active" => PseudoClass::Active,
            "focus" => PseudoClass::Focus,
            "focus-visible" => PseudoClass::FocusVisible,
            "focus-within" => PseudoClass::FocusWithin,
            "target" | "target-within" => PseudoClass::Target,
            "checked" => PseudoClass::Checked,
            "indeterminate" => PseudoClass::Indeterminate,
            "default" => PseudoClass::Default,
            "disabled" => PseudoClass::Disabled,
            "enabled" => PseudoClass::Enabled,
            "required" => PseudoClass::Required,
            "optional" => PseudoClass::Optional,
            "read-only" => PseudoClass::ReadOnly,
            "read-write" => PseudoClass::ReadWrite,
            "placeholder-shown" => PseudoClass::PlaceholderShown,
            "defined" => PseudoClass::Defined,
            "open" | "popover-open" => PseudoClass::Open,
            "valid" | "in-range" | "user-valid" => PseudoClass::Valid,
            "invalid" | "out-of-range" | "user-invalid" => PseudoClass::Invalid,
            "fullscreen" | "modal" | "autofill" | "-webkit-autofill" | "-webkit-full-screen" | "-webkit-full-screen-ancestor"
            | "picture-in-picture" | "playing" | "paused" | "seeking" | "buffering" | "stalled" | "muted" | "volume-locked"
            | "past" | "current" | "future" | "blank" | "scope-defined" | "local-link" | "user-action" => PseudoClass::Never(lower),
            // anything else is invalid, as in Chrome: the whole rule is dropped
            _ => return Err(unsupported()),
        })
    }

    fn parse_non_ts_functional_pseudo_class(
        &self,
        name: &str,
        parser: &mut cssparser::Parser<'i>,
        _after_part: bool,
    ) -> Result<PseudoClass, cssparser::ParseError<Self::Error>> {
        match name.to_ascii_lowercase().as_str() {
            "lang" => {
                let mut langs = Vec::new();
                loop {
                    langs.push(parser.expect_ident_or_string()?.as_ref().to_ascii_lowercase());
                    if parser.try_parse(|p| p.expect_comma()).is_err() {
                        break;
                    }
                }
                Ok(PseudoClass::Lang(langs.join(",")))
            }
            "dir" => Ok(PseudoClass::Dir(parser.expect_ident()?.as_ref().to_ascii_lowercase())),
            "state" => {
                parser.expect_ident()?;
                Ok(PseudoClass::Never("state".into()))
            }
            _ => Err(unsupported()),
        }
    }

    fn parse_pseudo_element(&self, name: &str) -> Result<PseudoElement, cssparser::ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "before" => PseudoElement::Before,
            "after" => PseudoElement::After,
            "marker" | "placeholder" | "selection" | "first-line" | "first-letter" | "backdrop" | "file-selector-button" | "cue"
            | "target-text" | "spelling-error" | "grammar-error" | "details-content" | "search-text" | "column" | "scroll-marker"
            | "scroll-marker-group" | "scroll-button" | "checkmark" | "picker-icon" | "view-transition" => PseudoElement::Other(lower),
            // Chrome accepts its own -webkit- pseudo-elements, no other prefix
            n if n.starts_with("-webkit-") || n.starts_with("-internal-") => PseudoElement::Other(lower),
            _ => {
                return Err(unsupported())
            }
        })
    }

    fn parse_functional_pseudo_element(
        &self,
        name: &str,
        parser: &mut cssparser::Parser<'i>,
    ) -> Result<PseudoElement, cssparser::ParseError<Self::Error>> {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "highlight" | "view-transition-group" | "view-transition-image-pair" | "view-transition-old" | "view-transition-new" | "picker" | "scroll-button" | "part" => {
                while parser.next().is_ok() {}
                Ok(PseudoElement::Other(lower))
            }
            _ => Err(unsupported()),
        }
    }
}

/// A parsed selector, with what the rule index needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub selector: selectors::parser::Selector<Simple>,
    /// the rightmost compound's most specific simple selector: id (0),
    /// class (1) or tag (2), for indexing rules
    pub key: Option<(u8, String)>,
    /// ::before (1) / ::after (2); 3 for one this engine never draws
    pub pseudo_element: u8,
}

/// Parse a selector list as CSS does: one invalid selector makes the whole
/// list invalid (None). Nested rules (`&`) are resolved by the caller.
pub fn parse_list(text: &str) -> Option<Vec<Parsed>> {
    let mut parser = cssparser::Parser::new(text);
    let list = parser
        .parse_entirely(|p| selectors::parser::SelectorList::parse(&SelectorParser, p, ParseRelative::No))
        .ok()?;
    Some(list.slice().iter().map(|s| describe(s.clone())).collect())
}

fn describe(selector: selectors::parser::Selector<Simple>) -> Parsed {
    use selectors::parser::Component;
    let pseudo_element = match selector.pseudo_element() {
        None => 0,
        Some(PseudoElement::Before) => 1,
        Some(PseudoElement::After) => 2,
        Some(PseudoElement::Other(_)) => 3,
    };
    let mut key: Option<(u8, String)> = None;
    for component in selector.iter() {
        let candidate = match component {
            Component::ID(id) => Some((0, id.0.clone())),
            Component::Class(class) => Some((1, class.0.clone())),
            Component::LocalName(name) => Some((2, name.lower_name.0.clone())),
            _ => None,
        };
        if let Some(c) = candidate {
            if key.as_ref().is_none_or(|k| c.0 < k.0) {
                key = Some(c);
            }
        }
    }
    Parsed { selector, key, pseudo_element }
}

// ---------------------------------------------------------------------------
// matching
// ---------------------------------------------------------------------------

thread_local! {
    /// node → its parent, for the chains being matched (addresses)
    static PARENTS: RefCell<HashMap<usize, usize, crate::css::FastHash>> = RefCell::new(HashMap::default());
    /// which pseudo-element (1 ::before, 2 ::after) a pseudo match asks for
    static WANT_PSEUDO: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

fn addr(node: &Node) -> usize {
    node as *const Node as usize
}

/// Enter `node`'s ancestor chain in the parent table (outermost first).
pub fn register(node: &Node, ancestors: &[&Node]) {
    PARENTS.with(|p| {
        let mut p = p.borrow_mut();
        if p.len() > 1 << 21 {
            p.clear();
        }
        // some callers' chains already end with the node itself: cut there,
        // or the node would be its own parent (and `a b` would never end)
        let ancestors = match ancestors.iter().position(|a| std::ptr::eq(*a, node)) {
            Some(at) => &ancestors[..at],
            None => ancestors,
        };
        let mut parent = 0usize;
        for a in ancestors {
            if addr(a) != parent {
                p.insert(addr(a), parent);
            }
            parent = addr(a);
        }
        p.insert(addr(node), parent);
    });
}

fn set_parent(child: &Node, parent: &Node) {
    if !std::ptr::eq(child, parent) {
        PARENTS.with(|p| {
            p.borrow_mut().insert(addr(child), addr(parent));
        });
    }
}

fn is_element(n: &Node) -> bool {
    n.text.is_none() && !n.tag.starts_with('#')
}

#[derive(Clone, Copy)]
pub struct El<'a>(pub &'a Node);

impl fmt::Debug for El<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}>", self.0.tag)
    }
}

impl<'a> El<'a> {
    fn parent(&self) -> Option<&'a Node> {
        let parent = PARENTS.with(|p| p.borrow().get(&addr(self.0)).copied()).unwrap_or(0);
        // SAFETY: the table holds only nodes of the tree being matched,
        // entered from live references during this match
        (parent != 0).then(|| unsafe { &*(parent as *const Node) })
    }

    fn sibling(&self, step: isize) -> Option<El<'a>> {
        let parent = self.parent()?;
        let kids = &parent.children;
        let at = kids.iter().position(|c| std::ptr::eq(c, self.0))? as isize;
        let mut i = at + step;
        while i >= 0 && (i as usize) < kids.len() {
            let c = &kids[i as usize];
            if is_element(c) {
                set_parent(c, parent);
                return Some(El(c));
            }
            i += step;
        }
        None
    }

    fn input_type(&self) -> String {
        self.0.attr("type").unwrap_or("text").to_ascii_lowercase()
    }

    fn is_form_control(&self) -> bool {
        matches!(self.0.tag.as_str(), "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset")
    }

    fn disabled(&self) -> bool {
        if !self.is_form_control() {
            return false;
        }
        if self.0.attr("disabled").is_some() {
            return true;
        }
        // inside a disabled fieldset (not its first legend)
        let mut cur = *self;
        while let Some(p) = cur.parent() {
            if p.tag == "fieldset" && p.attr("disabled").is_some() {
                return true;
            }
            cur = El(p);
        }
        false
    }

    fn editable(&self) -> bool {
        match self.0.tag.as_str() {
            "textarea" => self.0.attr("readonly").is_none() && !self.disabled(),
            "input" => {
                !matches!(self.input_type().as_str(), "hidden" | "button" | "submit" | "reset" | "image" | "checkbox" | "radio" | "range" | "color" | "file")
                    && self.0.attr("readonly").is_none()
                    && !self.disabled()
            }
            _ => self.0.attr("contenteditable").is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("plaintext-only")),
        }
    }

    fn lang(&self) -> Option<String> {
        let mut cur = Some(self.0);
        while let Some(n) = cur {
            if let Some(l) = n.attr("lang").or_else(|| n.attr("xml:lang")) {
                return Some(l.to_ascii_lowercase());
            }
            cur = El(n).parent();
        }
        None
    }
}

impl<'a> selectors::Element for El<'a> {
    type Impl = Simple;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.0)
    }

    fn parent_element(&self) -> Option<Self> {
        self.parent().filter(|p| is_element(p)).map(El)
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.sibling(-1)
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.sibling(1)
    }

    fn first_element_child(&self) -> Option<Self> {
        let child = self.0.children.iter().find(|c| is_element(c))?;
        set_parent(child, self.0);
        Some(El(child))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        true
    }

    fn has_local_name(&self, name: &CssStr) -> bool {
        self.0.tag.eq_ignore_ascii_case(name.as_str())
    }

    fn has_namespace(&self, _ns: &CssStr) -> bool {
        true
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.0.tag == other.0.tag
    }

    fn attr_matches(&self, _ns: &NamespaceConstraint<&CssStr>, local_name: &CssStr, operation: &AttrSelectorOperation<&CssStr>) -> bool {
        let name = local_name.as_str().to_ascii_lowercase();
        match self.0.attr(&name) {
            Some(value) => operation.eval_str(value),
            None => false,
        }
    }

    fn has_attr_in_no_namespace(&self, local_name: &CssStr) -> bool {
        self.0.attr(&local_name.as_str().to_ascii_lowercase()).is_some()
    }

    fn match_non_ts_pseudo_class(&self, pc: &PseudoClass, _context: &mut MatchingContext<'_, Simple>) -> bool {
        let n = self.0;
        match pc {
            PseudoClass::Link | PseudoClass::AnyLink => self.is_link(),
            PseudoClass::Visited => false,
            PseudoClass::Hover | PseudoClass::Active | PseudoClass::Focus | PseudoClass::FocusVisible | PseudoClass::FocusWithin | PseudoClass::Target => false,
            PseudoClass::Checked => match n.tag.as_str() {
                "input" => matches!(self.input_type().as_str(), "checkbox" | "radio") && n.attr("checked").is_some(),
                "option" => n.attr("selected").is_some(),
                _ => false,
            },
            PseudoClass::Indeterminate => false,
            PseudoClass::Default => n.attr("checked").is_some() || n.attr("selected").is_some(),
            PseudoClass::Disabled => self.disabled(),
            PseudoClass::Enabled => self.is_form_control() && !self.disabled(),
            PseudoClass::Required => matches!(n.tag.as_str(), "input" | "select" | "textarea") && n.attr("required").is_some(),
            PseudoClass::Optional => matches!(n.tag.as_str(), "input" | "select" | "textarea") && n.attr("required").is_none(),
            PseudoClass::ReadOnly => !self.editable(),
            PseudoClass::ReadWrite => self.editable(),
            PseudoClass::PlaceholderShown => {
                matches!(n.tag.as_str(), "input" | "textarea")
                    && n.attr("placeholder").is_some()
                    && if n.tag == "textarea" { n.text_content().is_empty() } else { n.attr("value").is_none_or(str::is_empty) }
            }
            PseudoClass::Defined => true,
            PseudoClass::Open => matches!(n.tag.as_str(), "details" | "dialog") && n.attr("open").is_some(),
            PseudoClass::Valid => true,
            PseudoClass::Invalid => false,
            PseudoClass::Lang(langs) => self.lang().is_some_and(|l| langs.split(',').any(|want| l == want || l.starts_with(&format!("{want}-")) || want == "*")),
            PseudoClass::Dir(d) => d == "ltr",
            PseudoClass::Never(_) => false,
        }
    }

    fn match_pseudo_element(&self, pe: &PseudoElement, _context: &mut MatchingContext<'_, Simple>) -> bool {
        let want = WANT_PSEUDO.with(|w| w.get());
        matches!((pe, want), (PseudoElement::Before, 1) | (PseudoElement::After, 2))
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        matches!(self.0.tag.as_str(), "a" | "area") && self.0.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        self.0.tag == "slot"
    }

    fn has_id(&self, id: &CssStr, case_sensitivity: CaseSensitivity) -> bool {
        self.0.attr("id").is_some_and(|have| case_sensitivity.eq(have.as_bytes(), id.as_str().as_bytes()))
    }

    fn has_class(&self, name: &CssStr, case_sensitivity: CaseSensitivity) -> bool {
        self.0.attr("class").is_some_and(|classes| classes.split_ascii_whitespace().any(|c| case_sensitivity.eq(c.as_bytes(), name.as_str().as_bytes())))
    }

    fn has_custom_state(&self, _name: &CssStr) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssStr) -> Option<CssStr> {
        None
    }

    fn is_part(&self, _name: &CssStr) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        // comments do not count; any text does
        !self.0.children.iter().any(|c| is_element(c) || c.text.as_deref().is_some_and(|t| !t.is_empty()))
    }

    fn is_root(&self) -> bool {
        self.0.tag == "html" && self.parent().is_none_or(|p| !is_element(p))
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

/// Does `selector` match `node` (whose chain is registered)?
pub fn matches(selector: &Parsed, node: &Node) -> bool {
    if selector.pseudo_element != 0 {
        return false;
    }
    let mut caches = SelectorCaches::default();
    let mut context = MatchingContext::new(MatchingMode::Normal, None, &mut caches, QuirksMode::NoQuirks, NeedsSelectorFlags::No, MatchingForInvalidation::No);
    selectors::matching::matches_selector(&selector.selector, 0, None, El(node), &mut context)
}

/// Does a `…::before` (1) / `…::after` (2) selector style `node`'s box?
pub fn matches_pseudo(selector: &Parsed, node: &Node, kind: u8) -> bool {
    if selector.pseudo_element != kind {
        return false;
    }
    WANT_PSEUDO.with(|w| w.set(kind));
    let mut caches = SelectorCaches::default();
    let mut context = MatchingContext::new(MatchingMode::ForStatelessPseudoElement, None, &mut caches, QuirksMode::NoQuirks, NeedsSelectorFlags::No, MatchingForInvalidation::No);
    let hit = selectors::matching::matches_selector(&selector.selector, 0, None, El(node), &mut context);
    WANT_PSEUDO.with(|w| w.set(0));
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(text: &str) -> Parsed {
        let mut list = parse_list(text).unwrap_or_else(|| panic!("parse {text}"));
        list.remove(0)
    }

    #[test]
    fn level_four_selectors_match() {
        let doc = crate::dom::parse_document(
            "<main><article class=card><h2 id=t>Title</h2><img alt=x></article><article class=card><h2>Plain</h2></article>\
             <ul><li>1</li><li class=x>2</li><li>3</li><li class=x>4</li></ul></main>",
        );
        let body = crate::dom::body(&doc);
        let main = &body.children[0];
        let html = &doc.children[0];
        let chain = |extra: &[&'static Node]| -> Vec<&Node> { let mut v: Vec<&Node> = vec![&doc, html, body, main]; v.extend_from_slice(extra); v };
        let first = &main.children[0];
        let second = &main.children[1];
        let check = |text: &str, node: &Node, ancestors: &[&Node]| {
            register(node, ancestors);
            matches(&sel(text), node)
        };
        // :has() with a complex argument
        assert!(check("article:has(> img)", first, &chain(&[])));
        assert!(!check("article:has(> img)", second, &chain(&[])));
        // :is() / :not() with complex lists
        let h2 = &first.children[0];
        let a: Vec<&Node> = { let mut v = chain(&[]); v.push(first); v };
        assert!(check(":is(main .card) > h2:not(.other, :empty)", h2, &a));
        assert!(check("h2:where(#t)", h2, &a));
        // :nth-child(An+B of S)
        let ul = &main.children[2];
        let ul_chain: Vec<&Node> = { let mut v = chain(&[]); v.push(ul); v };
        assert!(check("li:nth-child(2 of .x)", &ul.children[3], &ul_chain));
        assert!(!check("li:nth-child(2 of .x)", &ul.children[1], &ul_chain));
        // siblings
        assert!(check("li.x ~ li.x", &ul.children[3], &ul_chain));
        assert!(check("li + li.x", &ul.children[1], &ul_chain));
        // specificity: :where() adds none, :is() its most specific argument
        assert!(sel(":where(#t) h2").selector.specificity() < sel("#t").selector.specificity());
        assert_eq!(sel(":is(#t, .card) h2").selector.specificity(), sel("#t h2").selector.specificity());
        // shadow-tree selectors parse (Tailwind's `:root, :host`) and match
        // nothing in the document
        let list = parse_list(":root, :host").expect(":host is valid");
        let html_chain: Vec<&Node> = vec![&doc];
        register(html, &html_chain);
        assert!(matches(&list[0], html));
        assert!(!matches(&list[1], html));
        assert!(parse_list("x-card::part(label), ::slotted(p)").is_some());
    }

    #[test]
    fn invalid_lists_and_pseudo_elements() {
        // one unknown pseudo-class invalidates the list, as in Chrome
        assert!(parse_list("a, b:-moz-focusring").is_none());
        assert!(parse_list("a:hover, b::-webkit-scrollbar").is_some());
        assert_eq!(sel("p::before").pseudo_element, 1);
        assert_eq!(sel("p::after").pseudo_element, 2);
        assert_eq!(sel("p::placeholder").pseudo_element, 3);
        assert_eq!(sel("div.a#b").key, Some((0, "b".into())));
        assert_eq!(sel("div.a").key, Some((1, "a".into())));
    }
}
