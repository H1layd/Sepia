//! Other networks: Gemini, Gopher, and the overlay transports (Tor / I2P).
//!
//! Gemini is TLS + one line of request; Gopher is plain TCP + one line. Both are
//! small enough to implement honestly, and both are unreachable through the
//! clearnet DNS — which is exactly why they belong in this browser.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme, StreamOwned};

use crate::net::{dial_chain, Chain};
use crate::resolver::Resolver;
use crate::url::Url;

pub struct TextPage {
    pub title: String,
    pub html: String,
    pub note: String,
}

/// Geminispace is trust-on-first-use: most capsules use self-signed certs, so
/// strict WebPKI would make the network unreachable by design.
#[derive(Debug)]
struct TrustOnFirstUse;

impl ServerCertVerifier for TrustOnFirstUse {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
        ]
    }
}

fn tls_connect(
    host: &str,
    port: u16,
    chain: &Chain,
    resolver: Option<&Resolver>,
    timeout: Duration,
    insecure: bool,
) -> Result<StreamOwned<ClientConnection, TcpStream>, String> {
    let tcp = if chain.hops.is_empty() {
        let address = match resolver {
            Some(resolver) => resolver.resolve(host, chain, timeout)?.into_iter().next(),
            None => None,
        }
        .ok_or_else(|| format!("no address for {host}"))?;
        TcpStream::connect_timeout(&std::net::SocketAddr::new(address, port), timeout)
            .map_err(|e| format!("connect {host}:{port}: {e}"))?
    } else {
        dial_chain(chain, host, port, timeout)?
    };
    tcp.set_read_timeout(Some(timeout)).ok();
    tcp.set_write_timeout(Some(timeout)).ok();

    let config = if insecure {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustOnFirstUse))
            .with_no_client_auth()
    } else {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    let name = ServerName::try_from(host.to_string())
        .map_err(|e| format!("bad server name {host}: {e}"))?;
    let conn = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("tls setup: {e}"))?;
    Ok(StreamOwned::new(conn, tcp))
}

fn read_all(stream: &mut impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > limit {
                    return Err("response too large".into());
                }
            }
            Err(err) => return Err(format!("read: {err}")),
        }
    }
    Ok(out)
}

// --------------------------------------------------------------------------- //
// gemini
// --------------------------------------------------------------------------- //

pub fn fetch_gemini(
    url: &Url,
    chain: &Chain,
    resolver: Option<&Resolver>,
    timeout: Duration,
    insecure: bool,
) -> Result<TextPage, String> {
    let mut current = url.clone();
    for _ in 0..5 {
        let mut tls = tls_connect(
            &current.host,
            if current.port == 0 { 1965 } else { current.port },
            chain,
            resolver,
            timeout,
            insecure,
        )?;
        let request = format!("{}\r\n", current.to_string());
        tls.write_all(request.as_bytes())
            .and_then(|_| tls.flush())
            .map_err(|e| format!("gemini write: {e}"))?;
        let raw = read_all(&mut tls, 4 * 1024 * 1024)?;
        let (status, meta, body) = parse_gemini_response(&raw)?;
        match status {
            20 | 21 => {
                let mime = if meta.is_empty() { "text/gemini" } else { meta.as_str() };
                if !mime.starts_with("text/") {
                    let html = format!(
                        "<h1>{}</h1><p>Non-text response ({}) of {} bytes.</p>",
                        escape_html(&current.to_string()),
                        escape_html(mime),
                        body.len()
                    );
                    return Ok(TextPage {
                        title: current.to_string(),
                        html,
                        note: format!("gemini {} {}", status, mime),
                    });
                }
                let text = String::from_utf8_lossy(&body).to_string();
                return Ok(TextPage {
                    title: gemtext_title(&text).unwrap_or_else(|| current.to_string()),
                    html: render_gemtext(&current, &text),
                    note: format!("gemini {} {}", status, mime),
                });
            }
            30 | 31 => {
                let location = meta.trim();
                let next = current
                    .resolve(location)
                    .ok_or_else(|| format!("gemini redirect to unknown target {location:?}"))?;
                current = next;
            }
            10 | 11 => {
                return Ok(TextPage {
                    title: current.to_string(),
                    html: format!(
                        "<h1>{}</h1><p>This capsule asks for a client certificate ({}).</p>",
                        escape_html(&current.to_string()),
                        escape_html(&meta)
                    ),
                    note: format!("gemini {} (client cert required)", status),
                })
            }
            _ => {
                return Ok(TextPage {
                    title: current.to_string(),
                    html: format!(
                        "<h1>Gemini error</h1><p>Status {}: {}</p>",
                        status,
                        escape_html(&meta)
                    ),
                    note: format!("gemini {} {}", status, meta),
                })
            }
        }
    }
    Err("too many gemini redirects".into())
}

pub fn parse_gemini_response(raw: &[u8]) -> Result<(u8, String, Vec<u8>), String> {
    let newline = raw
        .windows(2)
        .position(|w| w == b"\r\n")
        .ok_or_else(|| "gemini response has no CRLF".to_string())?;
    let header = String::from_utf8_lossy(&raw[..newline]).to_string();
    let (status_text, meta) = header.split_once(' ').unwrap_or((header.as_str(), ""));
    let status: u8 = status_text
        .parse()
        .map_err(|_| format!("bad gemini status {status_text:?}"))?;
    Ok((status, meta.trim().to_string(), raw[newline + 2..].to_vec()))
}

fn gemtext_title(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line[2..].trim().to_string())
}

/// Gemtext to the subset of HTML this engine lays out.
pub fn render_gemtext(base: &Url, text: &str) -> String {
    let mut html = String::new();
    let mut in_pre = false;
    let mut in_list = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("```") {
            if in_pre {
                html.push_str("</pre>");
                in_pre = false;
            } else {
                html.push_str("<pre>");
                in_pre = true;
            }
            continue;
        }
        if in_pre {
            html.push_str(&escape_html(trimmed));
            html.push('\n');
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("=>") {
            if in_list {
                html.push_str("</ul>");
                in_list = false;
            }
            let rest = rest.trim();
            let (target, label) = match rest.split_once(char::is_whitespace) {
                Some((target, label)) => (target, label.trim()),
                None => (rest, rest),
            };
            let href = absolute_href(base, target);
            let shown = if label.is_empty() { target } else { label };
            html.push_str(&format!(
                "<p><a href=\"{}\">{}</a></p>",
                escape_html(&href),
                escape_html(shown)
            ));
            continue;
        }
        if in_list && !trimmed.starts_with("* ") {
            html.push_str("</ul>");
            in_list = false;
        }
        for (prefix, tag) in [("### ", "h3"), ("## ", "h2"), ("# ", "h1")] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if in_list {
                    html.push_str("</ul>");
                    in_list = false;
                }
                html.push_str(&format!("<{tag}>{}</{tag}>", escape_html(rest)));
                break;
            }
        }
        if trimmed.starts_with('*') || trimmed.starts_with("- ") {
            if !in_list {
                html.push_str("<ul>");
                in_list = true;
            }
            let item = trimmed.trim_start_matches('*').trim_start_matches('-').trim();
            html.push_str(&format!("<li>{}</li>", escape_html(item)));
            continue;
        }
        if trimmed.starts_with('>') {
            html.push_str(&format!(
                "<p><i>{}</i></p>",
                escape_html(trimmed.trim_start_matches('>').trim())
            ));
        } else if trimmed.is_empty() {
            html.push_str("<p></p>");
        } else if !trimmed.starts_with('#') {
            html.push_str(&format!("<p>{}</p>", escape_html(trimmed)));
        }
    }
    if in_pre {
        html.push_str("</pre>");
    }
    if in_list {
        html.push_str("</ul>");
    }
    html
}

fn absolute_href(base: &Url, target: &str) -> String {
    match base.resolve(target) {
        Some(url) => url.to_string(),
        None => target.to_string(),
    }
}

// --------------------------------------------------------------------------- //
// gopher
// --------------------------------------------------------------------------- //

pub fn fetch_gopher(
    url: &Url,
    chain: &Chain,
    resolver: Option<&Resolver>,
    timeout: Duration,
) -> Result<TextPage, String> {
    let mut selector = url.path.trim_start_matches('/').to_string();
    let item_type = selector.chars().next().unwrap_or('1');
    if selector.len() > 1 {
        selector = selector[1..].to_string();
    } else {
        selector.clear();
    }
    // "1/" is a directory listing, "0name" is a text file.
    if item_type == '0' {
        let body = gopher_request(&url.host, url.port, &selector, chain, resolver, timeout)?;
        let text = String::from_utf8_lossy(&body).to_string();
        return Ok(TextPage {
            title: format!("gopher://{}/{}", url.host, selector),
            html: format!("<h1>{}</h1><pre>{}</pre>",
                          escape_html(&url.host), escape_html(&text)),
            note: "gopher text".into(),
        });
    }
    let body = gopher_request(&url.host, url.port, &selector, chain, resolver, timeout)?;
    let menu = String::from_utf8_lossy(&body).to_string();
    let html = render_gopher_menu(url, &menu);
    Ok(TextPage {
        title: format!("gopher://{}/{}", url.host, url.host),
        html,
        note: "gopher menu".into(),
    })
}

fn gopher_request(
    host: &str,
    port: u16,
    selector: &str,
    chain: &Chain,
    resolver: Option<&Resolver>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let port = if port == 0 { 70 } else { port };
    let mut stream = if chain.hops.is_empty() {
        let address = match resolver {
            Some(resolver) => resolver.resolve(host, chain, timeout)?.into_iter().next(),
            None => None,
        }
        .ok_or_else(|| format!("no address for {host}"))?;
        TcpStream::connect_timeout(&std::net::SocketAddr::new(address, port), timeout)
            .map_err(|e| format!("connect {host}:{port}: {e}"))?
    } else {
        dial_chain(chain, host, port, timeout)?
    };
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    stream
        .write_all(format!("{selector}\r\n").as_bytes())
        .map_err(|e| format!("gopher write: {e}"))?;
    read_all(&mut stream, 4 * 1024 * 1024)
}

pub fn render_gopher_menu(base: &Url, menu: &str) -> String {
    let mut html = String::new();
    let mut host_title = format!("{} — gopher", base.host);
    html.push_str(&format!("<h1>{}</h1>", escape_html(&host_title)));
    for raw in menu.split("\r\n") {
        if raw.is_empty() {
            continue;
        }
        let mut fields = raw.split('\t');
        // Gopher puts the type character in front of the display string.
        let first = fields.next().unwrap_or("");
        let item_type = first.chars().next().unwrap_or('i');
        let display = &first[item_type.len_utf8()..];
        let selector = fields.next().unwrap_or("");
        let host = fields.next().unwrap_or(&base.host);
        let port = fields.next().unwrap_or("70");
        match item_type {
            'i' | '3' => {
                if !display.is_empty() {
                    html.push_str(&format!("<p>{}</p>", escape_html(display)));
                }
            }
            '1' | '7' => {
                let href = format!("gopher://{host}:{port}/1{selector}");
                html.push_str(&format!(
                    "<p><a href=\"{}\">{}</a></p>",
                    escape_html(&href),
                    escape_html(if display.is_empty() { selector } else { display })
                ));
            }
            '0' | '5' | '9' | 'h' | 'g' | 'I' => {
                let href = format!("gopher://{host}:{port}/{item_type}{selector}");
                html.push_str(&format!(
                    "<p><a href=\"{}\">{}</a></p>",
                    escape_html(&href),
                    escape_html(if display.is_empty() { selector } else { display })
                ));
            }
            _ => {
                if !display.is_empty() {
                    html.push_str(&format!("<p>{}</p>", escape_html(display)));
                }
            }
        }
    }
    let _ = &mut host_title;
    html
}

// --------------------------------------------------------------------------- //
// helpers
// --------------------------------------------------------------------------- //

pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Which overlay a URL needs, translated into the transport that provides it.
pub fn overlay_hint(url: &Url) -> Option<&'static str> {
    match url.needs_overlay() {
        Some("tor") => Some(
            "onion names live on Tor: start tor and use --via tor (socks5 127.0.0.1:9050)",
        ),
        Some("i2p") => Some(
            "i2p names live inside I2P: start the router and use --via i2p (http proxy 127.0.0.1:4444)",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gemini_header() {
        let (status, meta, body) = parse_gemini_response(b"20 text/gemini\r\n# hi\r\n").unwrap();
        assert_eq!(status, 20);
        assert_eq!(meta, "text/gemini");
        assert_eq!(body, b"# hi\r\n");
        assert!(parse_gemini_response(b"no crlf here").is_err());
        assert!(parse_gemini_response(b"xx nope\r\n").is_err());
    }

    #[test]
    fn renders_gemtext_structure() {
        let base = Url::parse("gemini://example.org/a").unwrap();
        let text = "# Title\nSome text\n=> /other link label\n* one\n* two\n```\nraw <code>\n```\n";
        let html = render_gemtext(&base, text);
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<p>Some text</p>"));
        assert!(html.contains("href=\"gemini://example.org/other\""));
        assert!(html.contains("<li>one</li>"));
        assert!(html.contains("<pre>"));
        assert!(html.contains("raw &lt;code&gt;"));
        assert_eq!(gemtext_title(text).as_deref(), Some("Title"));
    }

    #[test]
    fn gopher_menu_becomes_links() {
        let base = Url::parse("gopher://floodgap.com/1/").unwrap();
        let menu = "1Directory\t/\tfloodgap.com\t70\r\niInfo line\tfake\t(NULL)\t0\r\n0Read me\treadme.txt\tfloodgap.com\t70\r\n";
        let html = render_gopher_menu(&base, menu);
        assert!(html.contains("gopher://floodgap.com:70/1/"));
        assert!(html.contains("gopher://floodgap.com:70/0readme.txt"));
        assert!(html.contains("Info line"));
        assert!(html.contains("Read me"));
    }

    #[test]
    fn escapes_html() {
        assert_eq!(escape_html("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }

    #[test]
    fn overlay_hints() {
        let onion = Url::parse("http://x.onion/").unwrap();
        assert!(overlay_hint(&onion).unwrap().contains("--via tor"));
        let i2p = Url::parse("http://x.i2p/").unwrap();
        assert!(overlay_hint(&i2p).unwrap().contains("--via i2p"));
        assert!(overlay_hint(&Url::parse("https://example.com").unwrap()).is_none());
    }
}
