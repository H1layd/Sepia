//! HTTP/1.1 client with TLS and an optional SOCKS5 chain — written by hand.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopKind {
    Socks5,
    Http,
}

#[derive(Debug, Clone)]
pub struct Hop {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub kind: HopKind,
}

#[derive(Debug, Clone, Default)]
pub struct Chain {
    pub hops: Vec<Hop>,
}

impl Chain {
    pub fn parse(spec: &str) -> Result<Chain, String> {
        let mut hops = Vec::new();
        for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
            let raw = part.trim();
            let (kind, default_port, addr) = if let Some(rest) = raw.strip_prefix("socks5h://") {
                (HopKind::Socks5, 1080u16, rest)
            } else if let Some(rest) = raw.strip_prefix("socks5://") {
                (HopKind::Socks5, 1080, rest)
            } else if let Some(rest) = raw.strip_prefix("http://") {
                (HopKind::Http, 4444, rest)
            } else if raw.starts_with("https://") {
                return Err("TLS-wrapped proxies are not supported yet: use http:// for the I2P proxy".into());
            } else {
                (HopKind::Socks5, 1080, raw)
            };
            let addr = addr.trim_end_matches('/');
            let (host, port) = match addr.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    p.parse::<u16>().map_err(|_| format!("bad hop port: {p}"))?,
                ),
                None => (addr.to_string(), default_port),
            };
            hops.push(Hop { host, port, username: None, password: None, kind });
        }
        Ok(Chain { hops })
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub final_url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> String {
        self.header("content-type").unwrap_or("").to_lowercase()
    }
}

#[allow(dead_code)]
pub fn fetch(url: &Url, chain: &Chain, timeout: Duration) -> Result<Response, String> {
    fetch_with(url, chain, None, timeout)
}

/// Fetch with an optional DoH resolver for direct (non-chained) connections.
pub fn fetch_with(
    url: &Url,
    chain: &Chain,
    resolver: Option<&crate::resolver::Resolver>,
    timeout: Duration,
) -> Result<Response, String> {
    let mut current = url.clone();
    for _ in 0..6 {
        let response = fetch_once(&current, chain, resolver, timeout)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(location) = response.header("location").map(|v| v.to_string()) {
                match current.resolve(&location) {
                    Some(next) => {
                        current = next;
                        continue;
                    }
                    None => return Ok(response),
                }
            }
        }
        return Ok(response);
    }
    Err("too many redirects".into())
}

/// The complete set of headers this browser will ever send. Keeping the list in
/// one function makes it possible to test that nothing else (cookies, referer,
/// client hints, auth) sneaks into the request.
pub fn request_headers(url: &Url) -> Vec<(String, String)> {
    vec![
        ("Host".to_string(), url.host_header()),
        ("User-Agent".to_string(), "sepia-browser/0.1".to_string()),
        (
            "Accept".to_string(),
            "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8".to_string(),
        ),
        ("Accept-Encoding".to_string(), "identity".to_string()),
        ("Accept-Language".to_string(), "en-US,en;q=0.9".to_string()),
        ("Connection".to_string(), "keep-alive".to_string()),
    ]
}

pub fn build_request(url: &Url) -> String {
    let mut request = format!("GET {} HTTP/1.1\r\n", url.request_target());
    for (name, value) in request_headers(url) {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request
}

fn fetch_once(
    url: &Url,
    chain: &Chain,
    resolver: Option<&crate::resolver::Resolver>,
    timeout: Duration,
) -> Result<Response, String> {
    let last_is_http = chain
        .hops
        .last()
        .map(|hop| hop.kind == HopKind::Http)
        .unwrap_or(false);
    if last_is_http && url.scheme == "http" {
        return fetch_via_http_proxy(url, chain, timeout);
    }

    // reuse an open connection to this origin when there is one
    let key = pool_key(url, chain);
    if let Some(stream) = take_pooled(&key) {
        if let Ok((response, keep)) = exchange(stream, url) {
            if let Some(stream) = keep {
                put_pooled(key, stream);
            }
            return Ok(response);
        }
        // a stale connection the server had closed: open a fresh one
    }
    let stream = connect_through(url, chain, resolver, timeout)?;
    let (response, keep) = exchange(stream, url)?;
    if let Some(stream) = keep {
        put_pooled(key, stream);
    }
    Ok(response)
}

/// Open connections, by origin and path (scheme, host, port and the chain),
/// kept for `IDLE` seconds: a page's stylesheets, scripts and images reuse
/// one TLS connection instead of a handshake each. Origins never share one.
static POOL: OnceLock<Mutex<HashMap<String, Vec<(Transport, Instant)>>>> = OnceLock::new();
const IDLE: Duration = Duration::from_secs(30);
const PER_ORIGIN: usize = 6;

fn pool_key(url: &Url, chain: &Chain) -> String {
    let path: Vec<String> = chain.hops.iter().map(|h| format!("{}:{}", h.host, h.port)).collect();
    format!("{}://{}:{}|{}", url.scheme, url.host.to_ascii_lowercase(), url.port, path.join(","))
}

fn take_pooled(key: &str) -> Option<Transport> {
    let mut pool = POOL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let list = pool.get_mut(key)?;
    while let Some((stream, since)) = list.pop() {
        if since.elapsed() < IDLE {
            return Some(stream);
        }
    }
    None
}

fn put_pooled(key: String, stream: Transport) {
    let mut pool = POOL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let list = pool.entry(key).or_default();
    list.retain(|(_, since)| since.elapsed() < IDLE);
    if list.len() < PER_ORIGIN {
        list.push((stream, Instant::now()));
    }
}

/// Close every pooled connection (the burn button, leaving a session).
pub fn forget_connections() {
    if let Some(pool) = POOL.get() {
        pool.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// One request and its response over `stream`; the stream comes back when
/// it can carry another request.
fn exchange(mut stream: Transport, url: &Url) -> Result<(Response, Option<Transport>), String> {
    let request = build_request(url);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write failed: {e}"))?;
    stream.flush().ok();

    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let mut header_end = None;
    while header_end.is_none() {
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        header_end = find_header_end(&buf);
        if buf.len() > 256 * 1024 {
            return Err("response headers too large".into());
        }
    }
    let Some(end) = header_end else {
        return Err("no HTTP response headers".into());
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let mut body = buf[end + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut status_parts = status_line.split_whitespace();
    let _version = status_parts.next();
    let status: u16 = status_parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line: {status_line}"))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };

    let bodiless = (100..200).contains(&status) || status == 204 || status == 304;
    let content_length = header("content-length").and_then(|v| v.parse::<usize>().ok());
    let chunked = header("transfer-encoding").map(|v| v.to_lowercase().contains("chunked")).unwrap_or(false);
    let framed = bodiless || content_length.is_some() || chunked;
    let body = if bodiless {
        Vec::new()
    } else if let Some(length) = content_length {
        while body.len() < length {
            let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
            if body.len() > 32 * 1024 * 1024 {
                return Err("body too large".into());
            }
        }
        body.truncate(length);
        body
    } else if chunked {
        let mut raw = body;
        loop {
            if dechunk(&raw).is_some() {
                break;
            }
            let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if raw.len() > 32 * 1024 * 1024 {
                return Err("body too large".into());
            }
        }
        dechunk(&raw).unwrap_or(raw)
    } else {
        loop {
            let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
            if body.len() > 32 * 1024 * 1024 {
                return Err("body too large".into());
            }
        }
        body
    };

    // the connection can carry the next request when the response was
    // framed and the server did not ask to close it
    let closing = header("connection").is_some_and(|v| v.to_ascii_lowercase().contains("close"));
    let reusable = framed && !closing;
    Ok((
        Response { status, headers, body, final_url: url.to_string() },
        reusable.then_some(stream),
    ))
}

/// Plain-http request through an HTTP proxy: what I2P's proxy expects.
fn fetch_via_http_proxy(url: &Url, chain: &Chain, timeout: Duration) -> Result<Response, String> {
    let prefix = &chain.hops[..chain.hops.len() - 1];
    let proxy = chain.hops.last().expect("checked by caller");
    let mut stream = if prefix.is_empty() {
        tcp_connect(&proxy.host, proxy.port, timeout)?
    } else {
        open_tunnel(&Chain { hops: prefix.to_vec() }, &proxy.host, proxy.port, timeout)?
    };
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let absolute = format!("http://{}{}", url.host_header(), url.request_target());
    let mut request = format!(
        "GET {absolute} HTTP/1.1\r\nHost: {}\r\nUser-Agent: sepia-browser/0.1\r\n\
         Accept: text/html,*/*;q=0.8\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
        url.host_header()
    );
    if let Some(user) = &proxy.username {
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(format!("{user}:{}", proxy.password.clone().unwrap_or_default()).as_bytes())
        ));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("proxy write: {e}"))?;

    let (status, headers, body) = read_response(&mut stream)?;
    Ok(Response { status, headers, body, final_url: url.to_string() })
}

fn read_response(stream: &mut impl Read) -> Result<(u16, Vec<(String, String)>, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    let mut header_end = None;
    while header_end.is_none() {
        let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        header_end = find_header_end(&buf);
        if buf.len() > 256 * 1024 {
            return Err("response headers too large".into());
        }
    }
    let Some(end) = header_end else {
        return Err("no HTTP response headers".into());
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let mut body = buf[end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line: {status_line}"))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    if let Some(length) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        while body.len() < length {
            let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(length);
    } else if header("transfer-encoding").map(|v| v.to_lowercase().contains("chunked")).unwrap_or(false) {
        loop {
            if dechunk(&body).is_some() {
                break;
            }
            let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body = dechunk(&body).unwrap_or(body);
    } else {
        loop {
            let n = stream.read(&mut chunk).map_err(|e| format!("read failed: {e}"))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    }
    Ok((status, headers, body))
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let pos = data.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&data[..pos]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        data = &data[pos + 2..];
        if size == 0 {
            return Some(out);
        }
        if data.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

enum Transport {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(s) => s.read(buf),
            Transport::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(s) => s.write(buf),
            Transport::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Transport::Plain(s) => s.flush(),
            Transport::Tls(s) => s.flush(),
        }
    }
}

fn connect_through(
    url: &Url,
    chain: &Chain,
    resolver: Option<&crate::resolver::Resolver>,
    timeout: Duration,
) -> Result<Transport, String> {
    let tcp = if !chain.hops.is_empty() {
        open_tunnel(chain, &url.host, url.port, timeout)?
    } else if let Some(resolver) = resolver {
        // Direct path: the name never reaches the system resolver.
        let addresses = resolver.resolve(&url.host, chain, timeout)?;
        let mut last = String::from("no address");
        let mut connected = None;
        for address in addresses {
            match tcp_connect_addr(address, url.port, timeout) {
                Ok(stream) => {
                    connected = Some(stream);
                    break;
                }
                Err(err) => last = err,
            }
        }
        connected.ok_or_else(|| format!("connect failed via DoH: {last}"))?
    } else {
        tcp_connect(&url.host, url.port, timeout)?
    };
    tcp.set_read_timeout(Some(timeout)).ok();
    tcp.set_write_timeout(Some(timeout)).ok();

    if url.scheme == "https" {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = ServerName::try_from(url.host.clone())
            .map_err(|e| format!("bad server name: {e}"))?;
        let conn = ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("tls setup failed: {e}"))?;
        Ok(Transport::Tls(Box::new(StreamOwned::new(conn, tcp))))
    } else {
        Ok(Transport::Plain(tcp))
    }
}

/// Establish a tunnel to `host:port` through the chain, honouring hop kinds.
pub fn open_tunnel(chain: &Chain, host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let first = chain.hops.first().ok_or("empty chain")?;
    let mut stream = tcp_connect(&first.host, first.port, timeout)?;
    for (index, hop) in chain.hops.iter().enumerate() {
        let (target_host, target_port) = if index + 1 < chain.hops.len() {
            let next = &chain.hops[index + 1];
            (next.host.clone(), next.port)
        } else {
            (host.to_string(), port)
        };
        match hop.kind {
            HopKind::Socks5 => socks5_connect(
                &mut stream,
                &target_host,
                target_port,
                hop.username.as_deref(),
                hop.password.as_deref(),
            )?,
            HopKind::Http => http_connect(
                &mut stream,
                &target_host,
                target_port,
                hop.username.as_deref(),
                hop.password.as_deref(),
            )?,
        }
    }
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    Ok(stream)
}

fn http_connect(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(), String> {
    let authority = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(user) = username {
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(format!("{user}:{}", password.unwrap_or("")).as_bytes())
        ));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        if stream.read_exact(&mut byte).is_err() {
            return Err("proxy closed during CONNECT".into());
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 8192 {
            return Err("proxy response too large".into());
        }
    }
    let text = String::from_utf8_lossy(&head);
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "bad proxy response".to_string())?;
    if status != 200 {
        return Err(format!("proxy CONNECT rejected with {status}"));
    }
    Ok(())
}

pub fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

fn tcp_connect_addr(address: std::net::IpAddr, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    TcpStream::connect_timeout(&SocketAddr::new(address, port), timeout)
        .map_err(|e| format!("{address}:{port}: {e}"))
}

/// Connect to `host:port` through the chain; the first hop resolves names.
pub fn dial_chain(
    chain: &Chain,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<TcpStream, String> {
    open_tunnel(chain, host, port, timeout)
}

fn tcp_connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .collect();
    let mut last = String::from("no addresses");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    Err(format!("connect failed: {last}"))
}

fn socks5_connect(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(), String> {
    let methods: Vec<u8> = if username.is_some() { vec![0x02] } else { vec![0x00] };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(&methods);
    stream.write_all(&greeting).map_err(|e| e.to_string())?;

    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).map_err(|e| e.to_string())?;
    if reply[0] != 0x05 {
        return Err("hop is not SOCKS5".into());
    }
    if reply[1] == 0x02 {
        let user = username.unwrap_or("");
        let pass = password.unwrap_or("");
        let mut auth = vec![0x01, user.len() as u8];
        auth.extend_from_slice(user.as_bytes());
        auth.push(pass.len() as u8);
        auth.extend_from_slice(pass.as_bytes());
        stream.write_all(&auth).map_err(|e| e.to_string())?;
        let mut ar = [0u8; 2];
        stream.read_exact(&mut ar).map_err(|e| e.to_string())?;
        if ar[1] != 0 {
            return Err("hop rejected credentials".into());
        }
    } else if reply[1] != 0x00 {
        return Err("hop accepted no auth method".into());
    }

    let mut request = vec![0x05, 0x01, 0x00];
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        request.push(0x01);
        request.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        request.push(0x04);
        request.extend_from_slice(&ip.octets());
    } else {
        if host.len() > 255 {
            return Err("hostname too long".into());
        }
        request.push(0x03);
        request.push(host.len() as u8);
        request.extend_from_slice(host.as_bytes());
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).map_err(|e| e.to_string())?;

    let mut head = [0u8; 4];
    stream.read_exact(&mut head).map_err(|e| e.to_string())?;
    let skip = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).map_err(|e| e.to_string())?;
            len[0] as usize + 2
        }
        _ => return Err("bad SOCKS5 address type".into()),
    };
    let mut tail = vec![0u8; skip];
    stream.read_exact(&mut tail).map_err(|e| e.to_string())?;
    if head[1] != 0x00 {
        return Err(format!("SOCKS5 CONNECT failed with {}", head[1]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test --release fetch_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn fetch_timing() {
        let resolver = crate::resolver::Resolver::default();
        let chain = Chain::default();
        for address in ["https://news.ycombinator.com/", "https://news.ycombinator.com/news.css", "https://a152b.host/", "https://a152b.host/style.css"] {
            let url = Url::parse(address).unwrap();
            let t = std::time::Instant::now();
            let _ = resolver.resolve(&url.host, &chain, Duration::from_secs(10));
            let dns = t.elapsed();
            let t = std::time::Instant::now();
            let tcp = {
                let ips = resolver.resolve(&url.host, &chain, Duration::from_secs(10)).unwrap();
                tcp_connect_addr(ips[0], url.port, Duration::from_secs(10)).unwrap()
            };
            let connect = t.elapsed();
            drop(tcp);
            let t = std::time::Instant::now();
            let r = fetch_with(&url, &chain, Some(&resolver), Duration::from_secs(15)).unwrap();
            println!("{address}: dns {:.0} ms, tcp {:.0} ms, whole fetch {:.0} ms, {} bytes", dns.as_secs_f64() * 1e3, connect.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3, r.body.len());
        }
    }

    fn url(value: &str) -> Url {
        Url::parse(value).expect("url")
    }

    #[test]
    fn header_surface_is_minimal() {
        let headers = request_headers(&url("https://example.com/x?y=1"));
        let names: Vec<String> = headers.iter().map(|(k, _)| k.to_lowercase()).collect();
        assert_eq!(
            names,
            vec![
                "host",
                "user-agent",
                "accept",
                "accept-encoding",
                "accept-language",
                "connection"
            ]
        );
        for banned in ["cookie", "referer", "authorization", "sec-ch-ua", "if-none-match", "dnt"] {
            assert!(
                !names.iter().any(|name| name == banned),
                "{banned} must never be sent"
            );
        }
    }

    #[test]
    fn request_line_and_host_are_consistent() {
        let request = build_request(&url("http://example.com:8080/a?b=c"));
        assert!(request.starts_with("GET /a?b=c HTTP/1.1\r\n"));
        assert!(request.contains("Host: example.com:8080\r\n"));
        assert!(request.ends_with("\r\n\r\n"));
        assert!(!request.contains("Cookie"));
    }

    #[test]
    fn dechunks_body() {
        let body = b"4\r\nwiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(dechunk(body).unwrap(), b"wikipedia");
        assert!(dechunk(b"4\r\nwiki").is_none());
    }

    #[test]
    fn parses_chain_spec() {
        let chain = Chain::parse("127.0.0.1:9050,10.0.0.2:1080").unwrap();
        assert_eq!(chain.hops.len(), 2);
        assert_eq!(chain.hops[1].port, 1080);
        assert_eq!(Chain::parse("127.0.0.1").unwrap().hops[0].port, 1080);
    }

    #[test]
    fn finds_header_end() {
        assert_eq!(find_header_end(b"HTTP/1.1 200 OK\r\nA: b\r\n\r\nbody"), Some(21));
        assert_eq!(find_header_end(b"HTTP/1.1 200"), None);
    }
}
