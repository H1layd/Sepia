//! HTML tokenizer: tags, attributes, comments, raw-text elements, entities.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    StartTag { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    EndTag { name: String },
    Text(String),
    Comment(String),
    Doctype(String),
}

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
    "source", "track", "wbr",
];

const RAW_TEXT: &[&str] = &["script", "style", "title", "textarea"];

pub fn is_void(name: &str) -> bool {
    VOID_ELEMENTS.contains(&name)
}

pub fn tokenize(input: &str) -> Vec<Token> {
    let bytes: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut text = String::new();

    while i < bytes.len() {
        if bytes[i] != '<' {
            text.push(bytes[i]);
            i += 1;
            continue;
        }
        let rest: String = bytes[i..bytes.len().min(i + 12)].iter().collect();
        let rest_lower = rest.to_lowercase();

        if rest_lower.starts_with("<!--") {
            flush_text(&mut text, &mut tokens);
            let mut end = i + 4;
            let mut content = String::new();
            while end < bytes.len() {
                if end + 2 < bytes.len() && bytes[end] == '-' && bytes[end + 1] == '-' && bytes[end + 2] == '>' {
                    break;
                }
                content.push(bytes[end]);
                end += 1;
            }
            tokens.push(Token::Comment(content));
            i = (end + 3).min(bytes.len());
            continue;
        }
        if rest_lower.starts_with("<!") {
            flush_text(&mut text, &mut tokens);
            let mut end = i + 2;
            let mut content = String::new();
            while end < bytes.len() && bytes[end] != '>' {
                content.push(bytes[end]);
                end += 1;
            }
            tokens.push(Token::Doctype(content));
            i = (end + 1).min(bytes.len());
            continue;
        }
        if rest_lower.starts_with("</") {
            flush_text(&mut text, &mut tokens);
            let mut end = i + 2;
            let mut name = String::new();
            while end < bytes.len() && !bytes[end].is_whitespace() && bytes[end] != '>' {
                name.push(bytes[end]);
                end += 1;
            }
            while end < bytes.len() && bytes[end] != '>' {
                end += 1;
            }
            tokens.push(Token::EndTag { name: name.to_lowercase() });
            i = (end + 1).min(bytes.len());
            continue;
        }
        if i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphabetic() {
            flush_text(&mut text, &mut tokens);
            let mut j = i + 1;
            let mut name = String::new();
            while j < bytes.len() && !bytes[j].is_whitespace() && bytes[j] != '>' && bytes[j] != '/' {
                name.push(bytes[j]);
                j += 1;
            }
            let name = name.to_lowercase();
            let mut attrs = Vec::new();
            let mut self_closing = false;
            while j < bytes.len() {
                while j < bytes.len() && bytes[j].is_whitespace() {
                    j += 1;
                }
                if j >= bytes.len() {
                    break;
                }
                if bytes[j] == '>' {
                    break;
                }
                if bytes[j] == '/' && j + 1 < bytes.len() && bytes[j + 1] == '>' {
                    self_closing = true;
                    j += 1;
                    continue;
                }
                let mut attr_name = String::new();
                while j < bytes.len()
                    && !bytes[j].is_whitespace()
                    && bytes[j] != '='
                    && bytes[j] != '>'
                    && bytes[j] != '/'
                {
                    attr_name.push(bytes[j]);
                    j += 1;
                }
                let mut value = String::new();
                if j < bytes.len() && bytes[j] == '=' {
                    j += 1;
                    if j < bytes.len() && (bytes[j] == '"' || bytes[j] == '\'') {
                        let quote = bytes[j];
                        j += 1;
                        while j < bytes.len() && bytes[j] != quote {
                            value.push(bytes[j]);
                            j += 1;
                        }
                        j += 1;
                    } else {
                        while j < bytes.len() && !bytes[j].is_whitespace() && bytes[j] != '>' {
                            value.push(bytes[j]);
                            j += 1;
                        }
                    }
                }
                if !attr_name.is_empty() {
                    attrs.push((attr_name.to_lowercase(), decode_entities(&value)));
                }
            }
            tokens.push(Token::StartTag {
                name: name.clone(),
                attrs,
                self_closing: self_closing || is_void(&name),
            });
            i = (j + 1).min(bytes.len());

            if RAW_TEXT.contains(&name.as_str()) && name != "textarea" {
                let close = format!("</{name}");
                let mut end = i;
                let mut content = String::new();
                let lowered: Vec<char> = bytes[i..].iter().map(|c| c.to_ascii_lowercase()).collect();
                while end + close.len() <= bytes.len() {
                    let window: String = lowered[end - i..end - i + close.len()].iter().collect();
                    if window == close {
                        break;
                    }
                    content.push(bytes[end]);
                    end += 1;
                }
                if !content.is_empty() {
                    tokens.push(Token::Text(content));
                }
                let mut skip = end;
                while skip < bytes.len() && bytes[skip] != '>' {
                    skip += 1;
                }
                tokens.push(Token::EndTag { name });
                i = (skip + 1).min(bytes.len());
            }
            continue;
        }
        text.push(bytes[i]);
        i += 1;
    }
    flush_text(&mut text, &mut tokens);
    tokens
}

fn flush_text(text: &mut String, tokens: &mut Vec<Token>) {
    if text.is_empty() {
        return;
    }
    tokens.push(Token::Text(decode_entities(text)));
    text.clear();
}

pub fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '&' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut entity = String::new();
        while j < chars.len() && entity.len() < 10 && chars[j] != ';' && !chars[j].is_whitespace() {
            entity.push(chars[j]);
            j += 1;
        }
        if j < chars.len() && chars[j] == ';' {
            let decoded = match entity.as_str() {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                "copy" => Some('©'),
                "reg" => Some('®'),
                "trade" => Some('™'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "hellip" => Some('…'),
                "laquo" => Some('«'),
                "raquo" => Some('»'),
                "euro" => Some('€'),
                "pound" => Some('£'),
                "deg" => Some('°'),
                "middot" => Some('·'),
                "bull" => Some('•'),
                other if other.starts_with('#') => {
                    let numeric = &other[1..];
                    let code = if let Some(hex) = numeric.strip_prefix('x').or_else(|| numeric.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        numeric.parse::<u32>().ok()
                    };
                    code.and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                i = j + 1;
                continue;
            }
        }
        out.push('&');
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_attributes() {
        let tokens = tokenize(r#"<a href="x.html" class='big'>hi</a>"#);
        assert_eq!(tokens.len(), 3);
        match &tokens[0] {
            Token::StartTag { name, attrs, .. } => {
                assert_eq!(name, "a");
                assert_eq!(attrs[0], ("href".into(), "x.html".into()));
                assert_eq!(attrs[1], ("class".into(), "big".into()));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(tokens[1], Token::Text("hi".into()));
        assert_eq!(tokens[2], Token::EndTag { name: "a".into() });
    }

    #[test]
    fn void_and_self_closing() {
        let tokens = tokenize("<br><img src=x><hr/>");
        match &tokens[0] {
            Token::StartTag { self_closing, .. } => assert!(*self_closing),
            other => panic!("{other:?}"),
        }
        assert_eq!(tokens.len(), 3);
    }

    #[test]
    fn script_body_is_raw() {
        let tokens = tokenize("<script>if (a < b) { x = \"</p>\"; }</script>after");
        assert_eq!(tokens.len(), 4);
        match &tokens[1] {
            Token::Text(body) => assert!(body.contains("a < b")),
            other => panic!("{other:?}"),
        }
        assert_eq!(tokens[2], Token::EndTag { name: "script".into() });
        assert_eq!(tokens[3], Token::Text("after".into()));
    }

    #[test]
    fn entities_decode() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#65; &#x42;"), "a & b <c> A B");
        assert_eq!(decode_entities("&nbsp;"), "\u{a0}");
        assert_eq!(decode_entities("plain &"), "plain &");
        assert_eq!(decode_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn comments_are_skipped() {
        let tokens = tokenize("<div><!-- hidden -->x</div>");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[1], Token::Comment(" hidden ".into()));
        assert_eq!(tokens[2], Token::Text("x".into()));
    }
}
