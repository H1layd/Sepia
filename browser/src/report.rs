//! Sensors: every place the engine gives up on something a page uses —
//! an unknown CSS property or value, a selector or at-rule it cannot read,
//! a script error, an image or font it cannot decode, a load that failed.
//!
//! Off by default (one atomic load per call). `SEPIA_REPORT=path` turns it
//! on for a CLI run and writes one JSON object per line at the end:
//! `{"kind":"css-prop","key":"container-type","count":12}`.
//! `scripts/web-crawl.py` adds these up over many sites, so the most common
//! gaps in the whole web come first — that is what gets fixed next.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ON: AtomicBool = AtomicBool::new(false);
static SEEN: Mutex<Option<HashMap<(&'static str, String), u32>>> = Mutex::new(None);

pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Something of `kind` the engine did not handle; `key` names what.
pub fn miss(kind: &'static str, key: &str) {
    if !on() {
        return;
    }
    let key: String = key.trim().chars().take(120).collect();
    if key.is_empty() {
        return;
    }
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    *seen.get_or_insert_with(HashMap::new).entry((kind, key)).or_insert(0) += 1;
}

/// A CSS value the engine could not read: keyed by the function it uses
/// (`oklch(`, `color-mix(`) or, for a bare word, the word itself.
pub fn css_value(property: &str, value: &str) {
    if !on() {
        return;
    }
    let value = value.trim().to_ascii_lowercase();
    let head = match value.find('(') {
        Some(i) => format!("{}(", value[..i].rsplit(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("")),
        None => value.split_whitespace().next().unwrap_or("").to_string(),
    };
    miss("css-value", &format!("{property}: {head}"));
}

/// A selector the parser rejected: keyed by its pseudo-classes and
/// pseudo-elements (what usually makes it fail), else by the whole text.
pub fn css_selector(text: &str) {
    if !on() {
        return;
    }
    // a nested rule (`.a { &:hover {…} }`) or a declaration the rule
    // splitter took for a selector: CSS nesting, not a pseudo-class
    let t = text.trim();
    if t.contains('&') {
        return miss("css-selector", "nesting (&)");
    }
    let decl = t.split_once(':').is_some_and(|(name, value)| {
        let name = name.trim();
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic() || c == '-') && value.trim_start().starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '#')
    });
    if t.contains(';') || decl {
        return miss("css-selector", "nesting (declaration read as a selector)");
    }
    let mut pseudos: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':' {
            let start = i;
            while i < chars.len() && chars[i] == ':' {
                i += 1;
            }
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '-') {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
            if name.len() > 1 && !pseudos.contains(&name) {
                pseudos.push(name);
            }
        } else {
            i += 1;
        }
    }
    if pseudos.is_empty() {
        miss("css-selector", text);
    } else {
        for p in pseudos {
            miss("css-selector", &p);
        }
    }
}

/// A script error, as the engine words it: names a missing global when it
/// can (`ReferenceError: IntersectionObserver is not defined` →
/// `js-missing: IntersectionObserver`), otherwise the message with the
/// page-specific parts (quoted names, numbers) taken out.
pub fn js_error(line: &str) {
    if !on() {
        return;
    }
    const KINDS: [&str; 7] = ["ReferenceError", "TypeError", "SyntaxError", "RangeError", "URIError", "EvalError", "Error"];
    let Some((kind, at)) = KINDS.iter().find_map(|k| line.find(&format!("{k}:")).map(|at| (*k, at))) else { return };
    let message = line[at + kind.len() + 1..].lines().next().unwrap_or("").trim();
    if kind == "ReferenceError" {
        if let Some(name) = message.strip_suffix(" is not defined") {
            miss("js-missing", name.trim());
            return;
        }
    }
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut last_digit = false;
    for c in message.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                out.push(c);
            }
            Some(_) => {}
            None if c == '\'' || c == '"' || c == '`' => {
                quote = Some(c);
                out.push(c);
                out.push('…');
            }
            None if c.is_ascii_digit() => {
                if !last_digit {
                    out.push('N');
                }
            }
            None => out.push(c),
        }
        last_digit = c.is_ascii_digit();
    }
    let out: String = out.chars().take(100).collect();
    miss(if kind == "SyntaxError" { "js-syntax" } else { "js-error" }, &format!("{kind}: {out}"));
}

/// What a failed load was for and why, without the address.
pub fn load_failed(what: &str, error: &str) {
    if !on() {
        return;
    }
    // the kind of failure only: no host names, paths or numbers
    let error = error.split(|c: char| c == ':' || c == '(').next().unwrap_or(error);
    let words: Vec<&str> = error.split_whitespace().filter(|w| !w.contains(['.', '/']) && !w.chars().any(|c| c.is_ascii_digit())).collect();
    miss("load", &format!("{what}: {}", words.join(" ")));
}

/// Write everything seen as JSON lines, most frequent first.
pub fn write(path: &std::path::Path) -> std::io::Result<()> {
    let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&(&str, String), &u32)> = seen.as_ref().map(|m| m.iter().collect()).unwrap_or_default();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut out = String::new();
    for ((kind, key), count) in rows {
        out.push_str(&format!("{{\"kind\":{},\"key\":{},\"count\":{count}}}\n", json(kind), json(key)));
    }
    std::fs::write(path, out)
}

fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_what_the_engine_gave_up_on() {
        enable();
        js_error("app.js: ReferenceError: IntersectionObserver is not defined");
        js_error("timer error: TypeError: cannot read property 'foo' of undefined at 12:5");
        css_selector("a:has(> img)::before");
        css_value("color", "oklch(0.7 0.1 200)");
        let seen = SEEN.lock().unwrap();
        let seen = seen.as_ref().unwrap();
        assert!(seen.contains_key(&("js-missing", "IntersectionObserver".to_string())));
        assert!(seen.contains_key(&("js-error", "TypeError: cannot read property '…' of undefined at N:N".to_string())));
        assert!(seen.contains_key(&("css-selector", ":has".to_string())));
        assert!(seen.contains_key(&("css-selector", "::before".to_string())));
        assert!(seen.contains_key(&("css-value", "color: oklch(".to_string())));
    }
}
