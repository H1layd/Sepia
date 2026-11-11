//! Proxy chain: linear hops, optional auth, Linux egress marking.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::{ChainCfg, Hop};
use crate::socks5;

pub const EGRESS_MARK: u32 = 1;
#[cfg(target_os = "linux")]
pub const SO_MARK: i32 = 36;

#[derive(Debug, Clone)]
pub struct Chain {
    pub hops: Vec<Hop>,
    pub timeout: f64,
    pub on_failure: String,
}

impl Chain {
    pub fn new(cfg: &ChainCfg) -> Chain {
        Chain {
            hops: cfg.hops.clone(),
            timeout: cfg.connect_timeout.max(1.0),
            on_failure: cfg.on_hop_failure.clone(),
        }
    }

    pub fn depth(&self) -> usize {
        self.hops.len()
    }

    pub fn describe(&self) -> Vec<String> {
        self.hops
            .iter()
            .map(|h| format!("{} ({} {})", h.label(), h.kind, h.addr))
            .collect()
    }

    pub async fn dial(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        if self.hops.is_empty() {
            return connect_marked(host, port).await;
        }
        let mut limit = self.hops.len();
        loop {
            match self.dial_via(&self.hops[..limit], host, port).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    if self.on_failure == "skip" && limit > 1 {
                        limit -= 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn dial_via(&self, hops: &[Hop], host: &str, port: u16) -> io::Result<TcpStream> {
        let first = &hops[0];
        let (fh, fp) = first
            .parse_addr()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let mut stream = connect_marked(&fh, fp).await?;
        for (index, hop) in hops.iter().enumerate() {
            let (target_host, target_port) = if index + 1 < hops.len() {
                let next = &hops[index + 1];
                next.parse_addr()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?
            } else {
                (host.to_string(), port)
            };
            match hop.kind.as_str() {
                "http" => {
                    http_connect(
                        &mut stream,
                        &target_host,
                        target_port,
                        hop.username.as_deref(),
                        hop.password.as_deref(),
                    )
                    .await?
                }
                _ => {
                    socks5::client_connect(
                        &mut stream,
                        &target_host,
                        target_port,
                        hop.username.as_deref(),
                        hop.password.as_deref(),
                    )
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e.to_string()))?
                }
            }
        }
        Ok(stream)
    }
}

pub async fn connect_marked(host: &str, port: u16) -> io::Result<TcpStream> {
    let host = host.to_string();
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await?
        .collect();
    if addrs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no address"));
    }
    let mut last = None;
    for addr in addrs {
        let attempt = tokio::task::spawn_blocking(move || blocking_connect(addr))
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        match attempt {
            Ok(stream) => return stream.into_tokio(),
            Err(err) => last = Some(err),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("connect failed")))
}

#[derive(Debug)]
pub struct StdStream {
    inner: StdTcpStream,
}

impl StdStream {
    pub fn into_tokio(self) -> io::Result<TcpStream> {
        self.inner.set_nonblocking(true)?;
        TcpStream::from_std(self.inner)
    }
}

#[cfg(unix)]
fn blocking_connect(addr: SocketAddr) -> io::Result<StdStream> {
    use std::os::unix::io::FromRawFd;

    let domain = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    #[cfg(target_os = "linux")]
    {
        let mark: libc::c_uint = EGRESS_MARK;
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_MARK,
                &mark as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            // Not fatal: nftables then relies on the UID bypass instead.
            let _ = rc;
        }
    }

    let rc = unsafe {
        match addr {
            SocketAddr::V4(v4) => {
                let mut raw: libc::sockaddr_in = std::mem::zeroed();
                raw.sin_family = libc::AF_INET as libc::sa_family_t;
                raw.sin_port = v4.port().to_be();
                raw.sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
                libc::connect(
                    fd,
                    &raw as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
            SocketAddr::V6(v6) => {
                let mut raw: libc::sockaddr_in6 = std::mem::zeroed();
                raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                raw.sin6_port = v6.port().to_be();
                raw.sin6_addr.s6_addr = v6.ip().octets();
                libc::connect(
                    fd,
                    &raw as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }
    let inner = unsafe { StdTcpStream::from_raw_fd(fd) };
    Ok(StdStream { inner })
}

#[cfg(not(unix))]
fn blocking_connect(addr: SocketAddr) -> io::Result<StdStream> {
    Ok(StdStream { inner: StdTcpStream::connect(addr)? })
}

pub async fn http_connect(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> io::Result<()> {
    let authority = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(user) = username {
        let token = base64(format!("{user}:{}", password.unwrap_or("")).as_bytes());
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;

    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        if stream.read_exact(&mut byte).await.is_err() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "proxy closed"));
        }
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 8192 {
            return Err(io::Error::other("proxy response too large"));
        }
    }
    let text = String::from_utf8_lossy(&head);
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("bad HTTP proxy response"))?;
    if status != 200 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("HTTP CONNECT rejected with {status}"),
        ));
    }
    Ok(())
}

pub fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn chain_depth() {
        let cfg: ChainCfg = serde_json::from_str(r#"{"hops":[{"addr":"1.1.1.1:1080"}]}"#).unwrap();
        assert_eq!(Chain::new(&cfg).depth(), 1);
        assert!(Chain::new(&ChainCfg::default()).hops.is_empty());
    }
}

// Silence unused-import warnings on non-unix targets.
#[allow(dead_code)]
fn _unused(mut s: impl Write + Read) {
    let _ = s.write(&[]);
    let _ = s.read(&mut []);
}
