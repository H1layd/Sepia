//! Document tree built from tokens, with the forgiving bits real HTML needs.

use crate::html::{is_void, tokenize, Token};

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: Option<String>,
}

impl Node {
    pub fn element(tag: &str) -> Node {
        Node { tag: tag.to_string(), attrs: Vec::new(), children: Vec::new(), text: None }
    }

    pub fn text_node(value: &str) -> Node {
        Node {
            tag: "#text".into(),
            attrs: Vec::new(),
            children: Vec::new(),
            text: Some(value.to_string()),
        }
    }

    pub fn comment_node(value: &str) -> Node {
        Node { tag: "#comment".into(), attrs: vec![("__sepia_comment".into(), value.into())], children: Vec::new(), text: None }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    pub fn find_first(&self, tag: &str) -> Option<&Node> {
        if self.tag == tag {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.find_first(tag) {
                return Some(found);
            }
        }
        None
    }

    pub fn text_content(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        if let Some(text) = &self.text {
            out.push_str(text);
            return;
        }
        for child in &self.children {
            child.collect_text(out);
        }
    }
}

/// Tags that implicitly close an open element of the same kind.
fn implicit_close(open: &str, new_tag: &str) -> bool {
    match (open, new_tag) {
        ("p", "p") => true,
        ("li", "li") => true,
        ("td", "td") | ("td", "th") | ("th", "td") | ("th", "th") => true,
        ("option", "option") => true,
        (open, new) if open.starts_with('h') && open.len() == 2 && new.starts_with('h') => true,
        _ => false,
    }
}

pub fn parse_document(source: &str) -> Node {
    let tokens = tokenize(source);
    let mut root = Node::element("#document");
    let mut stack: Vec<Node> = Vec::new();

    fn append(stack: &mut Vec<Node>, root: &mut Node, node: Node) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => root.children.push(node),
        }
    }

    for token in tokens {
        match token {
            Token::StartTag { name, attrs, self_closing } => {
                // A block start closes an open paragraph, including its inline
                // descendants. Without this recovery later sections become
                // children of links/emphasis and inherit the wrong geometry.
                if matches!(name.as_str(), "address" | "article" | "aside" | "blockquote"
                    | "details" | "div" | "dl" | "fieldset" | "figcaption" | "figure"
                    | "footer" | "form" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "header" | "hgroup" | "hr" | "main" | "menu" | "nav" | "ol"
                    | "p" | "pre" | "section" | "table" | "ul") {
                    let in_scope = stack.iter().rposition(|n| matches!(n.tag.as_str(), "p" | "table" | "td" | "th" | "button" | "html"));
                    if let Some(at) = in_scope.filter(|&at| stack[at].tag == "p") {
                        while stack.len() > at {
                            let node = stack.pop().unwrap();
                            append(&mut stack, &mut root, node);
                        }
                    }
                }
                if let Some(top) = stack.last() {
                    if implicit_close(&top.tag, &name) {
                        if let Some(node) = stack.pop() {
                            append(&mut stack, &mut root, node);
                        }
                    }
                }
                let mut node = Node::element(&name);
                node.attrs = attrs;
                if self_closing || is_void(&name) {
                    append(&mut stack, &mut root, node);
                } else {
                    stack.push(node);
                }
            }
            Token::EndTag { name } => {
                // Close the nearest matching element; drop unmatched end tags.
                if let Some(position) = stack.iter().rposition(|n| n.tag == name) {
                    while stack.len() > position {
                        if let Some(node) = stack.pop() {
                            append(&mut stack, &mut root, node);
                        }
                    }
                }
            }
            Token::Text(text) => {
                if !text.is_empty() {
                    append(&mut stack, &mut root, Node::text_node(&text));
                }
            }
            Token::Comment(data) => append(&mut stack, &mut root, Node::comment_node(&data)),
            Token::Doctype(_) => {}
        }
    }
    while let Some(node) = stack.pop() {
        append(&mut stack, &mut root, node);
    }
    root
}

/// Body content, or the whole document when there is no body element.
#[allow(dead_code)] // kept for callers outside the engine
pub fn body(document: &Node) -> &Node {
    document.find_first("body").unwrap_or(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nests_elements() {
        let doc = parse_document("<div><p>a<b>c</b></p></div>");
        let div = doc.find_first("div").unwrap();
        let p = div.find_first("p").unwrap();
        assert_eq!(p.text_content(), "ac");
    }

    #[test]
    fn closes_paragraphs_implicitly() {
        let doc = parse_document("<p>one<p>two");
        let paragraphs: Vec<&Node> = doc.children.iter().filter(|n| n.tag == "p").collect();
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].text_content(), "one");
    }

    #[test]
    fn block_starts_close_paragraphs_through_inline_descendants() {
        let doc = parse_document("<p><em>intro<div>section</div><p>end");
        assert_eq!(doc.children.len(), 3);
        assert_eq!(doc.children[0].tag, "p");
        assert_eq!(doc.children[0].text_content(), "intro");
        assert_eq!(doc.children[1].tag, "div");
        assert_eq!(doc.children[1].text_content(), "section");
        assert_eq!(doc.children[2].text_content(), "end");
    }

    #[test]
    fn closes_list_items() {
        let doc = parse_document("<ul><li>a<li>b</ul>");
        let ul = doc.find_first("ul").unwrap();
        let items: Vec<&Node> = ul.children.iter().filter(|n| n.tag == "li").collect();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn ignores_stray_end_tags() {
        let doc = parse_document("</div><p>x</p>");
        assert_eq!(doc.text_content(), "x");
    }

    #[test]
    fn keeps_attributes() {
        let doc = parse_document(r#"<a href="/x" class="c">t</a>"#);
        let link = doc.find_first("a").unwrap();
        assert_eq!(link.attr("href"), Some("/x"));
        assert_eq!(link.attr("class"), Some("c"));
    }
}
