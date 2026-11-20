//! Document tree built from tokens, with the forgiving bits real HTML needs.


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

/// Parse a whole document with the HTML standard's tree construction
/// (html5ever, Servo's parser): misnested and unclosed tags, tables and
/// what is foster-parented out of them, implied html/head/body, raw text
/// in script/style/textarea, entities, `<template>` contents — as every
/// browser builds the same tree from the same bytes.
pub fn parse_document(source: &str) -> Node {
    use html5ever::tendril::TendrilSink;
    let dom = html5ever::parse_document(markup5ever_rcdom::RcDom::default(), Default::default()).one(source);
    let mut root = Node::element("#document");
    for child in dom.document.children.borrow().iter() {
        if let Some(node) = convert(child) {
            root.children.push(node);
        }
    }
    root
}

/// Parse markup set as an element's innerHTML: the fragment algorithm with
/// that element as context (`<tr>` inside a `<tbody>` stays a row).
pub fn parse_fragment(source: &str, context: &str) -> Vec<Node> {
    use html5ever::tendril::TendrilSink;
    use html5ever::{ns, LocalName, QualName};
    let context = if context.is_empty() || context.starts_with('#') { "body" } else { context };
    let name = QualName::new(None, ns!(html), LocalName::from(context));
    let dom = html5ever::parse_fragment(markup5ever_rcdom::RcDom::default(), Default::default(), name, Vec::new(), true).one(source);
    // the fragment parser puts its result under one <html> element
    let document = dom.document.children.borrow();
    let mut out = Vec::new();
    for top in document.iter() {
        let is_html = matches!(&top.data, markup5ever_rcdom::NodeData::Element { name, .. } if &*name.local == "html");
        if is_html {
            for child in top.children.borrow().iter() {
                out.extend(convert(child));
            }
        } else {
            out.extend(convert(top));
        }
    }
    out
}

/// One html5ever node as ours: names lowercased (the engine's convention),
/// namespaced attributes as `xlink:href`, template contents as children.
fn convert(handle: &markup5ever_rcdom::Handle) -> Option<Node> {
    use markup5ever_rcdom::NodeData;
    match &handle.data {
        NodeData::Text { contents } => Some(Node::text_node(&contents.borrow())),
        NodeData::Comment { contents } => Some(Node::comment_node(contents)),
        NodeData::Element { name, attrs, template_contents, .. } => {
            let mut node = Node::element(&name.local.to_ascii_lowercase().to_string());
            node.attrs = attrs
                .borrow()
                .iter()
                .map(|a| {
                    let local = a.name.local.to_ascii_lowercase().to_string();
                    let key = match a.name.prefix.as_deref() {
                        Some(prefix) => format!("{prefix}:{local}"),
                        None => local,
                    };
                    (key, a.value.to_string())
                })
                .collect();
            let contents = template_contents.borrow();
            let children = match contents.as_ref() {
                Some(fragment) => fragment.children.borrow().clone(),
                None => handle.children.borrow().clone(),
            };
            for child in children.iter() {
                if let Some(c) = convert(child) {
                    node.children.push(c);
                }
            }
            Some(node)
        }
        NodeData::Document => {
            let mut node = Node::element("#document");
            for child in handle.children.borrow().iter() {
                node.children.extend(convert(child));
            }
            Some(node)
        }
        NodeData::Doctype { .. } | NodeData::ProcessingInstruction { .. } => None,
    }
}

/// A page as the HTML parser leaves it in a browser: one `<html>` holding a
/// `<head>` and a `<body>`, created when the source leaves them out (scripts
/// expect `document.documentElement` and `document.body`). Metadata before
/// the first content goes to the head, the rest to the body.
pub fn complete_document(mut doc: Node) -> Node {
    let is_meta = |n: &Node| matches!(n.tag.as_str(), "meta" | "title" | "link" | "style" | "script" | "base" | "noscript" | "template")
        || n.text.as_deref().is_some_and(|t| t.trim().is_empty())
        || n.tag == "#comment";
    // everything under one <html>
    let html_at = doc.children.iter().position(|n| n.tag == "html");
    let mut html = match html_at {
        Some(i) => {
            let mut html = doc.children.remove(i);
            // stray nodes outside <html> (before or after it) belong inside it
            let outside: Vec<Node> = doc.children.drain(..).filter(|n| n.tag != "#comment").collect();
            let (before, after): (Vec<Node>, Vec<Node>) = {
                let mut b = Vec::new();
                let mut a = Vec::new();
                for (k, n) in outside.into_iter().enumerate() {
                    if k < i { b.push(n) } else { a.push(n) }
                }
                (b, a)
            };
            let mut kids = before;
            kids.append(&mut html.children);
            kids.extend(after);
            html.children = kids;
            html
        }
        None => {
            let mut html = Node::element("html");
            html.children = std::mem::take(&mut doc.children);
            html
        }
    };
    let has_body = html.children.iter().any(|n| matches!(n.tag.as_str(), "body" | "frameset"));
    let has_head = html.children.iter().any(|n| n.tag == "head");
    if !has_head || !has_body {
        let mut head = None;
        let mut body = None;
        let mut leading: Vec<Node> = Vec::new();
        let mut rest: Vec<Node> = Vec::new();
        let mut in_content = false;
        for n in std::mem::take(&mut html.children) {
            match n.tag.as_str() {
                "head" => head = Some(n),
                "body" | "frameset" => { in_content = true; body = Some(n); }
                _ if body.is_some() => {
                    // after </body>: into the body, as the parser does
                    if let Some(b) = body.as_mut() { b.children.push(n); }
                }
                _ if !in_content && is_meta(&n) && head.is_none() => leading.push(n),
                _ => { in_content = true; rest.push(n); }
            }
        }
        let mut head = head.unwrap_or_else(|| Node::element("head"));
        if !leading.is_empty() {
            head.children.extend(leading);
        }
        let mut body = body.unwrap_or_else(|| Node::element("body"));
        if !rest.is_empty() {
            let mut kids = rest;
            kids.append(&mut body.children);
            body.children = kids;
        }
        html.children = vec![head, body];
    }
    doc.children.push(html);
    doc
}

/// Body content, or the whole document when there is no body element.
pub fn body(document: &Node) -> &Node {
    document.find_first("body").unwrap_or(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_tree_the_html_standard_does() {
        // text and cells outside a table's cells are foster-parented before it
        let doc = parse_document("<table>stray<tr><td>cell</td></tr></table>");
        let b = body(&doc);
        assert_eq!(b.children[0].text.as_deref(), Some("stray"));
        assert_eq!(b.children[1].tag, "table");
        assert_eq!(b.children[1].find_first("tbody").map(|t| t.text_content()), Some("cell".into()));
        // misnested formatting: the adoption agency algorithm
        let doc = parse_document("<p><b>bold <i>both</b> italic</i></p>");
        let p = body(&doc).find_first("p").unwrap();
        assert_eq!(p.children[0].tag, "b");
        assert_eq!(p.children[1].tag, "i");
        assert_eq!(p.children[1].text_content(), " italic");
        // entities, including ones without a semicolon
        let doc = parse_document("<p>&copy; &amp;&nbsp;&#x41;&#66; &notit;</p>");
        assert_eq!(body(&doc).find_first("p").unwrap().text_content(), "© &\u{a0}AB ¬it;");
        // template contents stay with the template
        let doc = parse_document("<template id=t><li>a</li></template>");
        assert_eq!(doc.find_first("template").unwrap().children[0].tag, "li");
        // raw text: markup inside <textarea> and <title> is text
        let doc = parse_document("<title>a <b>b</b></title><textarea><p>x</p></textarea>");
        assert_eq!(doc.find_first("title").unwrap().text_content(), "a <b>b</b>");
        assert_eq!(doc.find_first("textarea").unwrap().text_content(), "<p>x</p>");
    }

    #[test]
    fn inner_html_is_parsed_in_its_element() {
        // rows set into a <tbody> stay rows (a document parse would drop them)
        let rows = parse_fragment("<tr><td>1</td></tr><tr><td>2</td></tr>", "tbody");
        assert_eq!(rows.iter().filter(|n| n.tag == "tr").count(), 2);
        let items = parse_fragment("<li>a<li>b", "ul");
        assert_eq!(items.iter().filter(|n| n.tag == "li").count(), 2);
    }

    #[test]
    fn documents_get_html_head_and_body() {
        let doc = complete_document(parse_document("<title>t</title><p>a</p>"));
        assert_eq!(doc.children.len(), 1);
        let html = &doc.children[0];
        assert_eq!(html.tag, "html");
        assert_eq!(html.children.iter().map(|n| n.tag.as_str()).collect::<Vec<_>>(), vec!["head", "body"]);
        assert_eq!(html.children[0].find_first("title").map(|t| t.text_content()), Some("t".into()));
        assert_eq!(html.children[1].find_first("p").map(|t| t.text_content()), Some("a".into()));
        // a full document stays as it is
        let full = complete_document(parse_document("<html><head><title>x</title></head><body><p>b</p></body></html>"));
        assert_eq!(full.children[0].children.len(), 2);
    }

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
        let paragraphs: Vec<&Node> = body(&doc).children.iter().filter(|n| n.tag == "p").collect();
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].text_content(), "one");
    }

    #[test]
    fn block_starts_close_paragraphs_through_inline_descendants() {
        let doc = parse_document("<p><em>intro<div>section</div><p>end");
        let b = body(&doc);
        assert_eq!(b.children.len(), 3);
        assert_eq!(b.children[0].tag, "p");
        assert_eq!(b.children[0].text_content(), "intro");
        assert_eq!(b.children[1].tag, "div");
        assert_eq!(b.children[1].text_content(), "section");
        assert_eq!(b.children[2].text_content(), "end");
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
