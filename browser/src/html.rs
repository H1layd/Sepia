//! HTML vocabulary the serializer needs (parsing is html5ever's, in dom.rs).

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr",
];

/// Elements with no end tag (`<br>`): serialized without one.
pub fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}
