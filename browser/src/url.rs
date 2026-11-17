//! URL parsing and resolution without any external crate.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub query: String,
}

impl Url {
    pub fn parse(input: &str) -> Option<Url> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }
        let (scheme, rest) = match input.split_once("://") {
            Some((s, r)) => {
                let scheme = s.to_lowercase();
                if !matches!(scheme.as_str(), "http" | "https" | "gemini" | "gopher") {
                    return None;
                }
                (scheme, r)
            }
            None => ("https".to_string(), input),
        };
        let (authority, tail) = match rest.find('/') {
            Some(pos) => (&rest[..pos], &rest[pos..]),
            None => (rest, "/"),
        };
        let (host, port) = split_authority(authority, &scheme)?;
        let (path, query) = match tail.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (tail.to_string(), String::new()),
        };
        Some(Url {
            scheme,
            host,
            port,
            path: if path.is_empty() { "/".into() } else { path },
            query,
        })
    }

    pub fn default_port(&self) -> u16 {
        scheme_default_port(&self.scheme)
    }

    /// Tor and I2P names cannot be resolved by a normal resolver: they need
    /// their own transport, so the UI has to say which one instead of failing
    /// with a generic DNS error.
    pub fn needs_overlay(&self) -> Option<&'static str> {
        let host = self.host.to_lowercase();
        if host.ends_with(".onion") {
            Some("tor")
        } else if host.ends_with(".i2p") {
            Some("i2p")
        } else {
            None
        }
    }

    pub fn host_header(&self) -> String {
        let default_port = self.default_port();
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn request_target(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }

    pub fn to_string(&self) -> String {
        let default_port = self.default_port();
        let authority = if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        format!("{}://{}{}", self.scheme, authority, self.request_target())
    }

    #[allow(dead_code)]
    pub fn origin(&self) -> String {
        format!("{}://{}:{}", self.scheme, self.host, self.port)
    }

    /// Resolve a href (relative or absolute) against this document URL.
    pub fn resolve(&self, href: &str) -> Option<Url> {
        let href = href.trim();
        if href.is_empty() {
            return None;
        }
        let lower = href.to_lowercase();
        for scheme in ["http://", "https://", "gemini://", "gopher://"] {
            if lower.starts_with(scheme) {
                return Url::parse(href);
            }
        }
        if lower.starts_with("mailto:") || lower.starts_with("javascript:")
            || lower.starts_with("tel:") || lower.starts_with("data:")
        {
            return None;
        }
        if let Some(rest) = href.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, rest));
        }
        let base_dir = match self.path.rfind('/') {
            Some(pos) => &self.path[..=pos],
            None => "/",
        };
        if href.starts_with('/') {
            return Url::parse(&format!("{}://{}{}", self.scheme, self.host_header(), href));
        }
        let joined = format!("{}{}", base_dir, href);
        Url::parse(&format!(
            "{}://{}{}",
            self.scheme,
            self.host_header(),
            normalise_path(&joined)
        ))
    }
}

pub fn scheme_default_port(scheme: &str) -> u16 {
    match scheme {
        "https" => 443,
        "gemini" => 1965,
        "gopher" => 70,
        _ => 80,
    }
}

fn split_authority(authority: &str, scheme: &str) -> Option<(String, u16)> {
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let default_port = scheme_default_port(scheme);
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Some((host.to_string(), port.parse().ok()?))
        }
        _ => {
            if authority.is_empty() {
                None
            } else {
                Some((authority.to_string(), default_port))
            }
        }
    }
}

fn normalise_path(path: &str) -> String {
    let (path_only, query) = match path.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string())),
        None => (path, None),
    };
    let mut parts: Vec<&str> = Vec::new();
    for segment in path_only.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    if path_only.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    if let Some(q) = query {
        out.push('?');
        out.push_str(&q);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute() {
        let url = Url::parse("https://example.com:8443/a/b?x=1").unwrap();
        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 8443);
        assert_eq!(url.path, "/a/b");
        assert_eq!(url.query, "x=1");
        assert_eq!(url.host_header(), "example.com:8443");
    }

    #[test]
    fn defaults_ports_and_path() {
        let url = Url::parse("http://example.com").unwrap();
        assert_eq!(url.port, 80);
        assert_eq!(url.path, "/");
        assert_eq!(Url::parse("example.com/x").unwrap().scheme, "https");
        assert!(Url::parse("ftp://example.com").is_none());
    }

    #[test]
    fn overlay_schemes_and_ports() {
        let gem = Url::parse("gemini://geminiprotocol.net/docs/").unwrap();
        assert_eq!(gem.scheme, "gemini");
        assert_eq!(gem.default_port(), 1965);
        assert_eq!(gem.host_header(), "geminiprotocol.net");
        let gopher = Url::parse("gopher://floodgap.com/1/").unwrap();
        assert_eq!(gopher.scheme, "gopher");
        assert_eq!(gopher.default_port(), 70);
        let onion = Url::parse("http://duckduckgogg42xjoc72x3sjasowoarfbgcmvfimaftt6twagswzczad.onion/").unwrap();
        assert_eq!(onion.needs_overlay(), Some("tor"));
        let i2p = Url::parse("http://stats.i2p/").unwrap();
        assert_eq!(i2p.needs_overlay(), Some("i2p"));
        assert_eq!(Url::parse("https://example.com").unwrap().needs_overlay(), None);
    }

    #[test]
    fn resolves_relatives() {
        let base = Url::parse("https://example.com/dir/page.html").unwrap();
        assert_eq!(
            base.resolve("other.html").unwrap().to_string(),
            "https://example.com/dir/other.html"
        );
        assert_eq!(
            base.resolve("/root.html").unwrap().to_string(),
            "https://example.com/root.html"
        );
        assert_eq!(
            base.resolve("//cdn.example.org/lib.js").unwrap().host,
            "cdn.example.org"
        );
        assert_eq!(
            base.resolve("../up.html").unwrap().to_string(),
            "https://example.com/up.html"
        );
        assert!(base.resolve("mailto:a@b.c").is_none());
    }

    #[test]
    fn collapses_dot_segments() {
        assert_eq!(normalise_path("/a/b/../c/./d"), "/a/c/d");
    }
}
