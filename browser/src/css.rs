//! CSS: a real (small) parser, selector matching, and the cascade.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLUE: Color = Color::rgb(0, 0, 238);
    pub const GRAY: Color = Color::rgb(128, 128, 128);
    pub const LIGHT_GRAY: Color = Color::rgb(238, 238, 238);

    pub fn with_alpha(self, a: u8) -> Color {
        Color { a, ..self }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }
}

pub fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim().to_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
                let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
                let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
                Some(Color::rgb(r, g, b))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color::rgb(r, g, b))
            }
            8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Color { r, g, b, a })
            }
            _ => None,
        };
    }
    if let Some(inner) = value.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
        if parts.len() == 3 {
            return Some(Color::rgb(
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            ));
        }
    }
    match value.as_str() {
        "black" => Some(Color::BLACK),
        "white" => Some(Color::WHITE),
        "red" => Some(Color::rgb(255, 0, 0)),
        "green" => Some(Color::rgb(0, 128, 0)),
        "blue" => Some(Color::BLUE),
        "yellow" => Some(Color::rgb(255, 255, 0)),
        "orange" => Some(Color::rgb(255, 165, 0)),
        "purple" => Some(Color::rgb(128, 0, 128)),
        "gray" | "grey" => Some(Color::GRAY),
        "silver" => Some(Color::rgb(192, 192, 192)),
        "lightgray" | "lightgrey" => Some(Color::LIGHT_GRAY),
        "darkgray" | "darkgrey" => Some(Color::rgb(169, 169, 169)),
        "navy" => Some(Color::rgb(0, 0, 128)),
        "teal" => Some(Color::rgb(0, 128, 128)),
        "maroon" => Some(Color::rgb(128, 0, 0)),
        "olive" => Some(Color::rgb(128, 128, 0)),
        "aqua" | "cyan" => Some(Color::rgb(0, 255, 255)),
        "fuchsia" | "magenta" => Some(Color::rgb(255, 0, 255)),
        "lime" => Some(Color::rgb(0, 255, 0)),
        "transparent" => Some(Color { r: 0, g: 0, b: 0, a: 0 }),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Descendant chain, outermost first: `div.card a` -> [div.card, a]
    pub chain: Vec<Compound>,
}

impl Selector {
    pub fn parse(text: &str) -> Option<Selector> {
        let text = text.trim();
        if text.is_empty() || text.starts_with('@') {
            return None;
        }
        let mut chain = Vec::new();
        for part in text.split_whitespace() {
            if part.contains('>') || part.contains('+') || part.contains('[') || part.contains(':') {
                // Combinators, attributes and pseudo classes are not supported;
                // skip the whole rule rather than mis-style the page.
                return None;
            }
            chain.push(parse_compound(part)?);
        }
        if chain.is_empty() {
            None
        } else {
            Some(Selector { chain })
        }
    }

    pub fn specificity(&self) -> u32 {
        self.chain
            .iter()
            .map(|c| {
                let mut score = 0;
                if c.id.is_some() {
                    score += 100;
                }
                score += 10 * c.classes.len() as u32;
                if c.tag.is_some() {
                    score += 1;
                }
                score
            })
            .sum()
    }
}

fn parse_compound(part: &str) -> Option<Compound> {
    let mut tag = None;
    let mut id = None;
    let mut classes = Vec::new();
    let mut token = String::new();
    let mut mode = 't';
    for ch in part.chars().chain(std::iter::once(' ')) {
        match ch {
            '.' | '#' => {
                if !token.is_empty() {
                    match mode {
                        't' => tag = Some(token.clone()),
                        '#' => id = Some(token.clone()),
                        _ => classes.push(token.clone()),
                    }
                    token.clear();
                }
                mode = ch;
            }
            ' ' => {
                if !token.is_empty() {
                    match mode {
                        't' => tag = Some(token.clone()),
                        '#' => id = Some(token.clone()),
                        _ => classes.push(token.clone()),
                    }
                    token.clear();
                }
            }
            '*' => {}
            other => token.push(other),
        }
    }
    if tag.as_deref() == Some("*") {
        tag = None;
    }
    if tag.is_none() && id.is_none() && classes.is_empty() {
        None
    } else {
        Some(Compound { tag, id, classes })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub selector: Selector,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

impl Stylesheet {
    pub fn parse(css: &str) -> Stylesheet {
        let mut sheet = Stylesheet::default();
        let mut rest = strip_comments(css);
        // Drop @media / @font-face blocks wholesale; keep their simple bodies out.
        rest = strip_at_blocks(&rest);
        let mut i = 0usize;
        let chars: Vec<char> = rest.chars().collect();
        while i < chars.len() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            let start = i;
            while i < chars.len() && chars[i] != '{' {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let selector_text: String = chars[start..i].iter().collect();
            i += 1;
            let body_start = i;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            let body: String = chars[body_start..i.saturating_sub(1)].iter().collect();
            let declarations = parse_declarations(&body);
            for selector_text_part in selector_text.split(',') {
                if let Some(selector) = Selector::parse(selector_text_part) {
                    sheet.rules.push(Rule { selector, declarations: declarations.clone() });
                }
            }
        }
        sheet
    }

    /// Declarations that apply to `node`, sorted by ascending specificity.
    pub fn matching(
        &self,
        node: &crate::dom::Node,
        ancestors: &[&crate::dom::Node],
    ) -> Vec<Declaration> {
        let mut matched: Vec<(u32, usize, Declaration)> = Vec::new();
        for (order, rule) in self.rules.iter().enumerate() {
            if matches_selector(&rule.selector, node, ancestors) {
                let score = rule.selector.specificity();
                for declaration in &rule.declarations {
                    matched.push((score, order, declaration.clone()));
                }
            }
        }
        matched.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        matched.into_iter().map(|(_, _, d)| d).collect()
    }
}

pub fn parse_declarations(body: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in body.split(';') {
        if let Some((name, value)) = part.split_once(':') {
            let name = name.trim().to_lowercase();
            let value = value.trim().to_string();
            if !name.is_empty() && !value.is_empty() {
                out.push(Declaration { name, value });
            }
        }
    }
    out
}

fn matches_selector(selector: &Selector, node: &crate::dom::Node, ancestors: &[&crate::dom::Node]) -> bool {
    if node.is_text() {
        return false;
    }
    let Some(last) = selector.chain.last() else {
        return false;
    };
    if !compound_matches(last, node) {
        return false;
    }
    match_prefix(&selector.chain[..selector.chain.len() - 1], ancestors)
}

/// A descendant chain matches if each compound has some ancestor, in order.
fn match_prefix(chain: &[Compound], ancestors: &[&crate::dom::Node]) -> bool {
    let Some(target) = chain.last() else {
        return true;
    };
    for index in (0..ancestors.len()).rev() {
        if compound_matches(target, ancestors[index])
            && match_prefix(&chain[..chain.len() - 1], &ancestors[..index])
        {
            return true;
        }
    }
    false
}

fn compound_matches(compound: &Compound, node: &crate::dom::Node) -> bool {
    if let Some(tag) = &compound.tag {
        if &node.tag != tag {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        if node.attr("id") != Some(id.as_str()) {
            return false;
        }
    }
    for class in &compound.classes {
        let classes = node.attr("class").unwrap_or("");
        if !classes.split_whitespace().any(|c| c == class) {
            return false;
        }
    }
    true
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let chars: Vec<char> = css.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn strip_at_blocks(css: &str) -> String {
    let chars: Vec<char> = css.chars().collect();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            while i < chars.len() && chars[i] != '{' && chars[i] != ';' {
                i += 1;
            }
            if i < chars.len() && chars[i] == ';' {
                i += 1;
                continue;
            }
            let mut depth = 0;
            while i < chars.len() {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_document;

    #[test]
    fn parses_rules_and_declarations() {
        let sheet = Stylesheet::parse(
            "body { color: red; margin: 0 } .note, #main { background-color: #eee }",
        );
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(sheet.rules[0].declarations[0].name, "color");
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    #[test]
    fn skips_unsupported_selectors_and_at_blocks() {
        let sheet = Stylesheet::parse(
            "@media screen { body { color: blue } } a:hover { color: red } a[href] { color: red } \
             p { color: green }",
        );
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selector.chain[0].tag.as_deref(), Some("p"));
    }

    #[test]
    fn specificity_ordering() {
        let sheet = Stylesheet::parse("p { color: red } .note { color: blue } #x { color: green }");
        let doc = parse_document(r#"<p id="x" class="note">t</p>"#);
        let node = doc.find_first("p").unwrap();
        let declarations = sheet.matching(node, &[&doc]);
        assert_eq!(declarations.last().unwrap().value, "green");
        assert_eq!(
            sheet.rules[0].selector.specificity(),
            1,
            "tag selector specificity"
        );
        assert_eq!(sheet.rules[2].selector.specificity(), 100);
    }

    #[test]
    fn descendant_matching() {
        let sheet = Stylesheet::parse("ul li { color: red } ol li { color: blue }");
        let doc = parse_document("<div><ul><li>in list</li></ul><ol><li>other</li></ol></div>");
        let ul = doc.find_first("ul").unwrap();
        let li = ul.find_first("li").unwrap();
        let matched = sheet.matching(li, &[&ul, &doc]);
        let colors: Vec<&str> = matched.iter().map(|d| d.value.as_str()).collect();
        assert!(colors.contains(&"red"));
        assert!(!colors.contains(&"blue"));
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#fff"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(parse_color("#1a2b3c"), Some(Color::rgb(26, 43, 60)));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(Color::rgb(1, 2, 3)));
        assert_eq!(parse_color("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("nope"), None);
    }
}
