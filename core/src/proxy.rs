//! SOCKS5 and HTTP proxy front ends, sharing one chain and one router.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::chain::Chain;
use crate::config::split_hostport;
use crate::rules::{Action, Router};
use crate::socks5;

#[derive(Debug, Default)]
pub struct Stats {
    pub started: Option<Instant>,
    pub connections: AtomicU64,
    pub active: AtomicU64,
    pub chained: AtomicU64,
    pub direct: AtomicU64,
    pub blocked: AtomicU64,
    pub failed: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    pub routes: Mutex<HashMap<String, u64>>,
    pub errors: Mutex<Vec<String>>,
}

impl Stats {
    pub fn new() -> Arc<Stats> {
        Arc::new(Stats {
            started: Some(Instant::now()),
            ..Default::default()
        })
    }

    pub fn uptime(&self) -> u64 {
        self.started.map(|s| s.elapsed().as_secs()).unwrap_or(0)
    }

    pub async fn note_route(&self, key: &str) {
        let mut routes = self.routes.lock().await;
        *routes.entry(key.to_string()).or_insert(0) += 1;
    }

    pub async fn note_error(&self, where_: &str, message: &str) {
        let mut errors = self.errors.lock().await;
        errors.push(format!("{where_}: {message}"));
        if errors.len() > 20 {
            let drain = errors.len() - 20;
            errors.drain(0..drain);
        }
    }

    pub async fn snapshot(&self) -> serde_json::Value {
        let mut routes: Vec<(String, u64)> = self
            .routes
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        routes.sort_by(|a, b| b.1.cmp(&a.1));
        routes.truncate(20);
        let errors: Vec<String> = self.errors.lock().await.clone();
        serde_json::json!({
            "uptime_s": self.uptime(),
            "connections": self.connections.load(Ordering::Relaxed),
            "active": self.active.load(Ordering::Relaxed),
            "chained": self.chained.load(Ordering::Relaxed),
            "direct": self.direct.load(Ordering::Relaxed),
            "blocked": self.blocked.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "bytes_up": self.bytes_up.load(Ordering::Relaxed),
            "bytes_down": self.bytes_down.load(Ordering::Relaxed),
            "per_route": routes.iter().map(|(k, v)| format!("{k} x{v}")).collect::<Vec<_>>(),
            "errors": errors,
        })
    }
}

pub struct Shared {
    pub chain: Chain,
    pub router: Router,
    pub stats: Arc<Stats>,
    pub socks_auth: Option<(String, String)>,
}

impl Shared {
    fn wants_auth(&self) -> bool {
        self.socks_auth.is_some()
    }

    async fn dial(&self, host: &str, port: u16) -> io::Result<(TcpStream, &'static str)> {
        match self.router.action_for(host) {
            Action::Block => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("blocked by rule: {host}"),
            )),
            Action::Direct => {
                let stream = crate::chain::connect_marked(host, port).await?;
                self.stats.direct.fetch_add(1, Ordering::Relaxed);
                self.stats.note_route(&format!("direct:{host}")).await;
                Ok((stream, "direct"))
            }
            Action::Chain if self.chain.depth() == 0 => {
                let stream = crate::chain::connect_marked(host, port).await?;
                self.stats.direct.fetch_add(1, Ordering::Relaxed);
                self.stats.note_route(&format!("direct:{host}")).await;
                Ok((stream, "direct"))
            }
            Action::Chain => {
                let stream = self.chain.dial(host, port).await?;
                self.stats.chained.fetch_add(1, Ordering::Relaxed);
                self.stats.note_route(&format!("chain:{host}")).await;
                Ok((stream, "chain"))
            }
        }
    }
}

pub async fn serve_socks(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    shared.stats.connections.fetch_add(1, Ordering::Relaxed);
                    shared.stats.active.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = handle_socks(stream, shared.clone()).await {
                        shared.stats.note_error("socks", &e.to_string()).await;
                    }
                    shared.stats.active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(_) => break,
        }
    }
}

async fn handle_socks(mut stream: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    let (user, pass) = match &shared.socks_auth {
        Some((u, p)) if shared.wants_auth() => (Some(u.clone()), Some(p.clone())),
        _ => (None, None),
    };
    let (host, port, command) =
        match socks5::server_handshake(&mut stream, user.as_deref(), pass.as_deref()).await {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
    if command != socks5::CMD_CONNECT {
        stream.write_all(&socks5::build_reply(0x07)).await?;
        return Ok(());
    }
    let dial = shared.dial(&host, port).await;
    let mut upstream = match dial {
        Ok((s, _route)) => s,
        Err(err) => {
            let code = match err.kind() {
                io::ErrorKind::PermissionDenied => 0x02,
                io::ErrorKind::ConnectionRefused => 0x05,
                _ => 0x01,
            };
            if err.kind() == io::ErrorKind::PermissionDenied {
                shared.stats.blocked.fetch_add(1, Ordering::Relaxed);
            } else {
                shared.stats.failed.fetch_add(1, Ordering::Relaxed);
            }
            stream.write_all(&socks5::build_reply(code)).await?;
            return Ok(());
        }
    };
    stream.write_all(&socks5::build_reply(0x00)).await?;
    let (up, down) = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    shared.stats.bytes_up.fetch_add(up, Ordering::Relaxed);
    shared.stats.bytes_down.fetch_add(down, Ordering::Relaxed);
    Ok(())
}

pub async fn serve_http(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    shared.stats.connections.fetch_add(1, Ordering::Relaxed);
                    shared.stats.active.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = handle_http(stream, shared.clone()).await {
                        shared.stats.note_error("http", &e.to_string()).await;
                    }
                    shared.stats.active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(_) => break,
        }
    }
}

fn bad_request(code: u16, message: &str) -> Vec<u8> {
    let body = format!("{{\"error\":\"{message}\",\"code\":{code}}}");
    format!(
        "HTTP/1.1 {code} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

async fn handle_http(mut stream: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        if stream.read_exact(&mut byte).await.is_err() {
            return Ok(());
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 32 * 1024 {
            stream.write_all(&bad_request(431, "headers too large")).await?;
            return Ok(());
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v)) => (m.to_string(), t.to_string(), v.to_string()),
        _ => {
            stream.write_all(&bad_request(400, "bad request line")).await?;
            return Ok(());
        }
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };

    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = split_hostport(&target) else {
            stream.write_all(&bad_request(400, "bad CONNECT target")).await?;
            return Ok(());
        };
        let mut upstream = match shared.dial(&host, port).await {
            Ok((s, _)) => s,
            Err(err) => {
                if err.kind() == io::ErrorKind::PermissionDenied {
                    shared.stats.blocked.fetch_add(1, Ordering::Relaxed);
                    stream.write_all(&bad_request(403, "destination blocked")).await?;
                } else {
                    shared.stats.failed.fetch_add(1, Ordering::Relaxed);
                    stream.write_all(&bad_request(502, "upstream connection failed")).await?;
                }
                return Ok(());
            }
        };
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        let (up, down) = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
        shared.stats.bytes_up.fetch_add(up, Ordering::Relaxed);
        shared.stats.bytes_down.fetch_add(down, Ordering::Relaxed);
        return Ok(());
    }

    // Absolute-URI form: rewrite to origin-form and relay.
    let (scheme, host, port, path) = if let Some(rest) = target.strip_prefix("http://") {
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a.to_string(), format!("/{p}")),
            None => (rest.to_string(), "/".to_string()),
        };
        match split_hostport(&authority) {
            Some((h, p)) => ("http".to_string(), h, p, path),
            None => ("http".to_string(), authority, 80u16, path),
        }
    } else {
        let Some(host_header) = header("host") else {
            stream.write_all(&bad_request(400, "no Host header")).await?;
            return Ok(());
        };
        match split_hostport(&host_header) {
            Some((h, p)) => ("http".to_string(), h, p, target.clone()),
            None => ("http".to_string(), host_header, 80u16, target.clone()),
        }
    };
    let mut upstream = match shared.dial(&host, port).await {
        Ok((s, _)) => s,
        Err(err) => {
            if err.kind() == io::ErrorKind::PermissionDenied {
                shared.stats.blocked.fetch_add(1, Ordering::Relaxed);
                stream.write_all(&bad_request(403, "destination blocked")).await?;
            } else {
                shared.stats.failed.fetch_add(1, Ordering::Relaxed);
                stream.write_all(&bad_request(502, "upstream connection failed")).await?;
            }
            return Ok(());
        }
    };
    let _ = scheme;

    let mut forwarded = format!("{method} {path} {version}\r\n");
    for (name, value) in &headers {
        let lower = name.to_lowercase();
        if lower == "proxy-connection" || lower == "proxy-authorization" {
            continue;
        }
        forwarded.push_str(&format!("{name}: {value}\r\n"));
    }
    forwarded.push_str("Connection: close\r\n\r\n");
    upstream.write_all(forwarded.as_bytes()).await?;

    let (up, down) = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    shared.stats.bytes_up.fetch_add(up, Ordering::Relaxed);
    shared.stats.bytes_down.fetch_add(down, Ordering::Relaxed);
    Ok(())
}

/// Linux REDIRECT recovery; other platforms have no original destination.
#[cfg(target_os = "linux")]
pub fn original_destination(stream: &TcpStream) -> Option<(String, u16)> {
    use std::os::unix::io::AsRawFd;

    let fd = stream.as_raw_fd();
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_IP,
            80, // SO_ORIGINAL_DST
            &mut addr as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    let ip = std::net::Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
    Some((ip.to_string(), u16::from_be(addr.sin_port)))
}

#[cfg(not(target_os = "linux"))]
pub fn original_destination(_stream: &TcpStream) -> Option<(String, u16)> {
    None
}
