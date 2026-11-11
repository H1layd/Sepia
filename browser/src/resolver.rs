//! DNS over HTTPS, written here: wire format + TLS POST through the chain.
//!
//! The point is that a direct connection never asks the system resolver for a
//! name. Without this, "anonymous browser" would still broadcast every hostname
//! to the ISP before the TLS handshake even starts.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::net::Chain;

pub const DEFAULT_DOH: &str = "https://cloudflare-dns.com/dns-query";
pub const DEFAULT_BOOTSTRAPS: &[&str] = &["1.1.1.1", "1.0.0.1"];

#[derive(Debug, Clone)]
pub struct Resolver {
    pub url: String,
    pub bootstraps: Vec<String>,
}

impl Default for Resolver {
    fn default() -> Resolver {
        Resolver {
            url: DEFAULT_DOH.to_string(),
            bootstraps: DEFAULT_BOOTSTRAPS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// How long a DoH answer is reused (memory only, gone when the browser quits).
const DNS_TTL: Duration = Duration::from_secs(300);

type Answers = HashMap<String, (Vec<IpAddr>, Instant)>;
static CACHE: OnceLock<Mutex<Answers>> = OnceLock::new();
static IN_FLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

/// Drop every remembered answer (the burn button).
pub fn forget() {
    if let Some(cache) = CACHE.get() {
        cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

fn cached(key: &str) -> Option<Vec<IpAddr>> {
    let cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    cache.get(key).filter(|(_, at)| at.elapsed() < DNS_TTL).map(|(ips, _)| ips.clone())
}

impl Resolver {
    pub fn new(url: Option<&str>) -> Resolver {
        match url {
            Some(url) if !url.is_empty() => Resolver {
                url: url.to_string(),
                bootstraps: DEFAULT_BOOTSTRAPS.iter().map(|s| s.to_string()).collect(),
            },
            _ => Resolver::default(),
        }
    }

    /// Resolve `host` without touching the system resolver. Answers are
    /// kept in memory for a few minutes (never on disk), one lookup per
    /// name at a time: the page, its stylesheets, scripts and images share it.
    pub fn resolve(&self, host: &str, chain: &Chain, timeout: Duration) -> Result<Vec<IpAddr>, String> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        let key = format!("{}|{}", self.url, host.to_ascii_lowercase());
        if let Some(hit) = cached(&key) {
            return Ok(hit);
        }
        // the same name asked from many threads at once: one DoH lookup
        let gate = {
            let mut flights = IN_FLIGHT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
            flights.entry(key.clone()).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
        };
        let _turn = gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cached(&key) {
            return Ok(hit);
        }
        let addresses = self.lookup(host, chain, timeout)?;
        CACHE
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, (addresses.clone(), Instant::now()));
        Ok(addresses)
    }

    fn lookup(&self, host: &str, chain: &Chain, timeout: Duration) -> Result<Vec<IpAddr>, String> {
        let endpoint = Endpoint::parse(&self.url)?;
        let mut addresses: Vec<IpAddr> = Vec::new();
        let mut last_error = String::from("no bootstrap reachable");

        // IPv4 first; AAAA only for names without an A record. An answer
        // with no records is an answer: it is not retried on another bootstrap.
        for qtype in [TYPE_A, TYPE_AAAA] {
            let query = build_query(host, qtype)?;
            for bootstrap in &self.bootstraps {
                match self.post(&endpoint, bootstrap, &query, chain, timeout) {
                    Ok(response) => {
                        addresses.extend(parse_answers(&response));
                        break;
                    }
                    Err(err) => last_error = err,
                }
            }
            if !addresses.is_empty() {
                break;
            }
        }
        if addresses.is_empty() {
            return Err(format!("DoH failed for {host}: {last_error}"));
        }
        addresses.dedup();
        if std::env::var("SEPIA_VERBOSE").or_else(|_| std::env::var("VEIL_VERBOSE")).is_ok() {
            let list: Vec<String> = addresses.iter().map(|ip| ip.to_string()).collect();
            eprintln!("doh {} -> {}", host, list.join(", "));
        }
        Ok(addresses)
    }

    fn post(
        &self,
        endpoint: &Endpoint,
        bootstrap: &str,
        wire: &[u8],
        chain: &Chain,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        let tcp = if chain.hops.is_empty() {
            connect_ip(bootstrap, endpoint.port, timeout)?
        } else {
            crate::net::dial_chain(chain, bootstrap, endpoint.port, timeout)?
        };

        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = ServerName::try_from(endpoint.host.clone())
            .map_err(|e| format!("bad DoH server name: {e}"))?;
        let conn = ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("tls setup: {e}"))?;
        let mut tls = StreamOwned::new(conn, tcp);
        tls.sock.set_read_timeout(Some(timeout)).ok();
        tls.sock.set_write_timeout(Some(timeout)).ok();

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/dns-message\r\n\
             Accept: application/dns-message\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            endpoint.path,
            endpoint.host,
            wire.len()
        );
        tls.write_all(request.as_bytes())
            .and_then(|_| tls.write_all(wire))
            .and_then(|_| tls.flush())
            .map_err(|e| format!("DoH write: {e}"))?;

        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match tls.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&chunk[..n]);
                    if raw.len() > 128 * 1024 {
                        return Err("DoH response too large".into());
                    }
                    if let Some(body) = split_http(&raw) {
                        return Ok(body);
                    }
                }
                Err(err) => return Err(format!("DoH read: {err}")),
            }
        }
        split_http(&raw).ok_or_else(|| "empty DoH response".into())
    }
}

struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

impl Endpoint {
    fn parse(url: &str) -> Result<Endpoint, String> {
        let rest = url
            .strip_prefix("https://")
            .ok_or_else(|| format!("DoH must be https: {url}"))?;
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a, format!("/{p}")),
            None => (rest, "/dns-query".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().unwrap_or(443)),
            None => (authority.to_string(), 443),
        };
        Ok(Endpoint { host, port, path })
    }
}

fn split_http(raw: &[u8]) -> Option<Vec<u8>> {
    let pos = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..pos]).to_lowercase();
    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        return None;
    }
    let body = &raw[pos + 4..];
    let mut length: Option<usize> = None;
    for line in head.lines() {
        if let Some(value) = line.strip_prefix("content-length:") {
            length = value.trim().parse().ok();
        }
    }
    match length {
        Some(expected) if body.len() >= expected => Some(body[..expected].to_vec()),
        Some(_) => None,
        None => Some(body.to_vec()),
    }
}

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_CNAME: u16 = 5;

pub fn build_query(host: &str, qtype: u16) -> Result<Vec<u8>, String> {
    let mut wire = Vec::new();
    let id = (std::process::id() as u16) ^ 0x5157;
    wire.extend_from_slice(&id.to_be_bytes());
    wire.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    wire.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    wire.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // an/ns/ar
    for label in host.trim_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("bad label in {host}"));
        }
        wire.push(label.len() as u8);
        wire.extend_from_slice(label.as_bytes());
    }
    wire.push(0);
    wire.extend_from_slice(&qtype.to_be_bytes());
    wire.extend_from_slice(&1u16.to_be_bytes()); // IN
    Ok(wire)
}

pub fn parse_answers(data: &[u8]) -> Vec<IpAddr> {
    let mut out = Vec::new();
    if data.len() < 12 {
        return out;
    }
    let qd = u16::from_be_bytes([data[4], data[5]]) as usize;
    let an = u16::from_be_bytes([data[6], data[7]]) as usize;
    let mut offset = 12;
    for _ in 0..qd {
        match skip_name(data, offset) {
            Some(next) => offset = next + 4,
            None => return out,
        }
    }
    for _ in 0..an {
        let Some(next) = skip_name(data, offset) else {
            return out;
        };
        offset = next;
        if offset + 10 > data.len() {
            return out;
        }
        let rtype = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let rdlen = u16::from_be_bytes([data[offset + 8], data[offset + 9]]) as usize;
        offset += 10;
        if offset + rdlen > data.len() {
            return out;
        }
        let rdata = &data[offset..offset + rdlen];
        match (rtype, rdlen) {
            (TYPE_A, 4) => out.push(IpAddr::V4(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]))),
            (TYPE_AAAA, 16) => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(rdata);
                out.push(IpAddr::V6(Ipv6Addr::from(octets)));
            }
            (TYPE_CNAME, _) => { /* chained answers follow; keep walking */ }
            _ => {}
        }
        offset += rdlen;
    }
    out
}

fn skip_name(data: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        if offset >= data.len() {
            return None;
        }
        let len = data[offset];
        if len == 0 {
            return Some(offset + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(offset + 2);
        }
        offset += 1 + len as usize;
    }
}

fn connect_ip(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve bootstrap {host}: {e}"))?
        .collect();
    let mut last = String::from("no addresses");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(err) => last = format!("{addr}: {err}"),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_answers_and_expires() {
        let key = "test|cached.example".to_string();
        CACHE.get_or_init(Default::default).lock().unwrap().insert(key.clone(), (vec![IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))], Instant::now()));
        assert_eq!(cached(&key), Some(vec![IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))]));
        let old = Instant::now() - DNS_TTL - Duration::from_secs(1);
        CACHE.get_or_init(Default::default).lock().unwrap().insert(key.clone(), (vec![], old));
        assert_eq!(cached(&key), None, "expired");
    }

    /// `cargo test --release resolve_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn resolve_timing() {
        let r = Resolver::default();
        for host in ["a152b.host", "a152b.host", "news.ycombinator.com"] {
            let t = std::time::Instant::now();
            let got = r.resolve(host, &Chain::default(), Duration::from_secs(10));
            println!("{host}: {:?} in {:.0} ms", got.map(|v| v.len()), t.elapsed().as_secs_f64() * 1000.0);
        }
    }

    #[test]
    fn builds_query_wire() {
        let wire = build_query("example.com", TYPE_A).expect("query");
        assert_eq!(&wire[0..2], &((std::process::id() as u16) ^ 0x5157).to_be_bytes());
        assert_eq!(&wire[2..4], &0x0100u16.to_be_bytes());
        assert_eq!(&wire[4..6], &1u16.to_be_bytes());
        assert_eq!(wire[12], 7);
        assert_eq!(&wire[13..20], b"example");
        assert_eq!(wire[20], 3);
        assert_eq!(&wire[21..24], b"com");
        assert_eq!(wire[24], 0, "root label terminates the name");
        assert_eq!(&wire[25..27], &TYPE_A.to_be_bytes());
        assert_eq!(&wire[27..29], &1u16.to_be_bytes(), "class IN");
    }

    #[test]
    fn rejects_bad_labels() {
        assert!(build_query("", TYPE_A).is_err());
        assert!(build_query(&"a".repeat(64), TYPE_A).is_err());
    }

    #[test]
    fn parses_answers() {
        // hand-built response: one A record and one AAAA record
        let mut data = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 2, 0, 0, 0, 0];
        data.extend_from_slice(&[7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 3, b'c', b'o', b'm', 0]);
        data.extend_from_slice(&TYPE_A.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        for (rtype, rdata) in [
            (TYPE_A, vec![93, 184, 215, 14]),
            (TYPE_AAAA, vec![
                0x26, 0x06, 0x28, 0x00, 0x02, 0x20, 0x00, 0x01,
                0x02, 0x48, 0x18, 0x93, 0x25, 0xc8, 0x19, 0x46,
            ]),
        ] {
            data.extend_from_slice(&[0xC0, 0x0C]);
            data.extend_from_slice(&rtype.to_be_bytes());
            data.extend_from_slice(&1u16.to_be_bytes());
            data.extend_from_slice(&60u32.to_be_bytes());
            data.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
            data.extend_from_slice(&rdata);
        }
        let addresses = parse_answers(&data);
        assert_eq!(addresses.len(), 2);
        assert_eq!(addresses[0].to_string(), "93.184.215.14");
        assert!(addresses[1].is_ipv6());
    }

    #[test]
    fn parses_endpoint() {
        let endpoint = Endpoint::parse("https://cloudflare-dns.com/dns-query").expect("endpoint");
        assert_eq!(endpoint.host, "cloudflare-dns.com");
        assert_eq!(endpoint.port, 443);
        assert_eq!(endpoint.path, "/dns-query");
        assert!(Endpoint::parse("http://x/y").is_err());
    }
}
