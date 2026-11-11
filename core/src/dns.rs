//! DNS: wire format, cache, and DoH-over-the-chain resolution.
//!
//! The DoH endpoint is only ever dialled by pinned bootstrap IP: resolving its
//! name through ourselves would deadlock (that query would come back to us).

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use crate::chain::Chain;
use crate::config::DnsCfg;

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_OPT: u16 = 41;
pub const RCODE_NOERROR: u8 = 0;
pub const RCODE_SERVFAIL: u8 = 2;
pub const RCODE_NXDOMAIN: u8 = 3;

#[derive(Debug, Clone)]
pub struct Query {
    pub id: u16,
    pub flags: u16,
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
    pub question_end: usize,
    pub udp_size: usize,
}

#[derive(Debug)]
pub struct DnsError(pub String);

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn err(msg: impl Into<String>) -> DnsError {
    DnsError(msg.into())
}

pub fn parse_query(data: &[u8]) -> Result<Query, DnsError> {
    if data.len() < 12 {
        return Err(err("short packet"));
    }
    let id = u16::from_be_bytes([data[0], data[1]]);
    let flags = u16::from_be_bytes([data[2], data[3]]);
    let qdcount = u16::from_be_bytes([data[4], data[5]]);
    let arcount = u16::from_be_bytes([data[10], data[11]]);
    if qdcount == 0 {
        return Err(err("no question"));
    }
    let (name, offset) = parse_name(data, 12)?;
    if offset + 4 > data.len() {
        return Err(err("truncated question"));
    }
    let qtype = u16::from_be_bytes([data[offset], data[offset + 1]]);
    let qclass = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
    let question_end = offset + 4;
    let udp_size = edns_udp_size(data, question_end, arcount);
    Ok(Query { id, flags, name, qtype, qclass, question_end, udp_size })
}

pub fn parse_name(data: &[u8], mut offset: usize) -> Result<(String, usize), DnsError> {
    let mut labels: Vec<String> = Vec::new();
    let mut next = offset;
    let mut jumped = false;
    let mut hops = 0;
    loop {
        if offset >= data.len() {
            return Err(err("name past end"));
        }
        let len = data[offset];
        if len == 0 {
            offset += 1;
            if !jumped {
                next = offset;
            }
            break;
        }
        if len & 0xC0 == 0xC0 {
            if offset + 1 >= data.len() {
                return Err(err("truncated pointer"));
            }
            let pointer = (((len & 0x3F) as usize) << 8) | data[offset + 1] as usize;
            if !jumped {
                next = offset + 2;
            }
            jumped = true;
            hops += 1;
            if hops > 32 {
                return Err(err("compression loop"));
            }
            offset = pointer;
            continue;
        }
        if len & 0xC0 != 0 {
            return Err(err("reserved label type"));
        }
        offset += 1;
        let end = offset + len as usize;
        if end > data.len() {
            return Err(err("truncated label"));
        }
        labels.push(String::from_utf8_lossy(&data[offset..end]).into_owned());
        offset = end;
    }
    Ok((labels.join("."), next))
}

pub fn encode_name(name: &str) -> Result<Vec<u8>, DnsError> {
    let mut out = Vec::with_capacity(name.len() + 2);
    for label in name.trim_matches('.').split('.') {
        if label.is_empty() {
            continue;
        }
        let raw = label.as_bytes();
        if raw.len() > 63 {
            return Err(err("label too long"));
        }
        out.push(raw.len() as u8);
        out.extend_from_slice(raw);
    }
    out.push(0);
    Ok(out)
}

fn edns_udp_size(data: &[u8], mut offset: usize, arcount: u16) -> usize {
    if arcount == 0 {
        return 512;
    }
    for _ in 0..arcount {
        let Ok((_name, next)) = parse_name(data, offset) else {
            return 512;
        };
        offset = next;
        if offset + 10 > data.len() {
            return 512;
        }
        let rtype = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let rclass = u16::from_be_bytes([data[offset + 2], data[offset + 3]]);
        let rdlen = u16::from_be_bytes([data[offset + 8], data[offset + 9]]) as usize;
        offset += 10;
        if rtype == TYPE_OPT {
            return (rclass as usize).max(512);
        }
        offset += rdlen;
    }
    512
}

pub fn build_address_response(query: &Query, address: IpAddr, ttl: u32) -> Vec<u8> {
    let matching = matches!(
        (query.qtype, address),
        (TYPE_A, IpAddr::V4(_)) | (TYPE_AAAA, IpAddr::V6(_))
    );
    let records: Vec<(String, u16, u32, Vec<u8>)> = if matching {
        let rdata = match address {
            IpAddr::V4(ip) => ip.octets().to_vec(),
            IpAddr::V6(ip) => ip.octets().to_vec(),
        };
        vec![(query.name.clone(), query.qtype, ttl, rdata)]
    } else {
        Vec::new()
    };
    build_response(query, RCODE_NOERROR, &records)
}

pub fn build_response(
    query: &Query,
    rcode: u8,
    records: &[(String, u16, u32, Vec<u8>)],
) -> Vec<u8> {
    let mut flags = 0x8000u16 | 0x0080 | (query.flags & 0x0100);
    flags = (flags & 0xFFF0) | (rcode as u16 & 0xF);
    let mut out = Vec::with_capacity(64 + records.len() * 32);
    out.extend_from_slice(&query.id.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&encode_name(&query.name).unwrap_or_default());
    out.extend_from_slice(&query.qtype.to_be_bytes());
    out.extend_from_slice(&query.qclass.to_be_bytes());
    for (name, rtype, ttl, rdata) in records {
        out.extend_from_slice(&encode_name(name).unwrap_or_default());
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&ttl.to_be_bytes());
        out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        out.extend_from_slice(rdata);
    }
    out
}

pub fn build_error(query: &Query, rcode: u8) -> Vec<u8> {
    build_response(query, rcode, &[])
}

pub fn rcode_of(data: &[u8]) -> u8 {
    if data.len() < 4 {
        return RCODE_SERVFAIL;
    }
    (u16::from_be_bytes([data[2], data[3]]) & 0xF) as u8
}

pub fn scan_ttl(data: &[u8]) -> u32 {
    let Ok(qdcount) = (|| -> Result<u16, DnsError> {
        Ok(u16::from_be_bytes([data[4], data[5]]))
    })() else {
        return 0;
    };
    let counts = [
        u16::from_be_bytes([data[6], data[7]]),
        u16::from_be_bytes([data[8], data[9]]),
        u16::from_be_bytes([data[10], data[11]]),
    ];
    let mut offset = 12;
    for _ in 0..qdcount {
        match parse_name(data, offset) {
            Ok((_n, next)) => offset = next + 4,
            Err(_) => return 0,
        }
    }
    let mut ttl: Option<u32> = None;
    for count in counts {
        for _ in 0..count {
            let Ok((_name, next)) = parse_name(data, offset) else {
                return ttl.unwrap_or(0);
            };
            offset = next;
            if offset + 10 > data.len() {
                return ttl.unwrap_or(0);
            }
            let rtype = u16::from_be_bytes([data[offset], data[offset + 1]]);
            let rec_ttl = u32::from_be_bytes([
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            let rdlen = u16::from_be_bytes([data[offset + 8], data[offset + 9]]) as usize;
            offset += 10 + rdlen;
            if rtype == TYPE_OPT {
                continue;
            }
            ttl = Some(ttl.map_or(rec_ttl, |t| t.min(rec_ttl)));
        }
    }
    ttl.unwrap_or(0)
}

pub fn patch_id(response: &[u8], id: u16) -> Vec<u8> {
    let mut out = response.to_vec();
    if out.len() >= 2 {
        out[0..2].copy_from_slice(&id.to_be_bytes());
    }
    out
}

// --------------------------------------------------------------------------- //
// resolver
// --------------------------------------------------------------------------- //

type Cache = Arc<Mutex<HashMap<String, (Vec<u8>, Instant)>>>;  // value, expiry

#[derive(Clone)]
pub struct Resolver {
    cfg: DnsCfg,
    chain: Chain,
    cache: Cache,
    connector: TlsConnector,
    block: Vec<String>,
}

impl Resolver {
    pub fn new(cfg: &DnsCfg, chain: Chain) -> Resolver {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Resolver {
            cfg: cfg.clone(),
            chain,
            cache: Arc::new(Mutex::new(HashMap::new())),
            connector: TlsConnector::from(Arc::new(tls)),
            block: cfg.block.iter().map(|d| d.to_lowercase()).collect(),
        }
    }

    pub fn is_blocked(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_lowercase();
        self.block
            .iter()
            .any(|pattern| crate::rules::match_domain(pattern, &name))
    }

    pub async fn resolve(&self, wire: &[u8]) -> Vec<u8> {
        let Ok(query) = parse_query(wire) else {
            return Vec::new();
        };
        if self.is_blocked(&query.name) {
            let address = match self.cfg.block_response.as_str() {
                "::" | "::1" => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
                "127.0.0.1" => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                _ => IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            };
            return build_address_response(&query, address, 60);
        }

        let key = format!(
            "{}|{}|{}",
            query.name.to_lowercase(),
            query.qtype,
            query.qclass
        );
        {
            let cache = self.cache.lock().await;
            if let Some((response, expires)) = cache.get(&key) {
                if Instant::now() < *expires {
                    return patch_id(response, query.id);
                }
            }
        }

        for server in &self.cfg.servers {
            for attempt in 0..2 {
                match self.doh_call(server, wire).await {
                    Ok(response) if !response.is_empty() => {
                        let rcode = rcode_of(&response);
                        if rcode <= 3 {
                            self.store(&key, &response).await;
                            return response;
                        }
                    }
                    Ok(_) => {}
                    Err(_e) => {
                        if attempt == 0 {
                            tokio::time::sleep(Duration::from_millis(120)).await;
                            continue;
                        }
                    }
                }
            }
        }
        build_error(&query, RCODE_SERVFAIL)
    }

    async fn store(&self, key: &str, response: &[u8]) {
        let mut ttl = scan_ttl(response);
        if rcode_of(response) != RCODE_NOERROR {
            ttl = 30;
        }
        let max_ttl = self.cfg.cache_ttl_max.clamp(1, u32::MAX as u64) as u32;
        let ttl = ttl.clamp(1, max_ttl);
        let expires = Instant::now() + Duration::from_secs(ttl as u64);
        let mut cache = self.cache.lock().await;
        if cache.len() > 5000 {
            let now = Instant::now();
            cache.retain(|_, (_, expiry)| now < *expiry);
            if cache.len() > 5000 {
                cache.clear();
            }
        }
        cache.insert(key.to_string(), (response.to_vec(), expires));
    }

    async fn doh_call(&self, server: &crate::config::DnsServer, wire: &[u8]) -> io::Result<Vec<u8>> {
        let host = server
            .url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        let path = server
            .url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split_once('/'))
            .map(|(_, p)| format!("/{p}"))
            .unwrap_or_else(|| "/dns-query".to_string());
        if host.is_empty() {
            return Err(io::Error::other("bad DoH url"));
        }
        let mut last_error = io::Error::other("no bootstrap addresses");
        let attempts: Vec<String> = if server.bootstraps.is_empty() {
            vec![host.clone()]
        } else {
            server.bootstraps.clone()
        };
        for pinned in attempts {
            match self.doh_once(&host, &path, &pinned, wire).await {
                Ok(body) => return Ok(body),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    async fn doh_once(&self, host: &str, path: &str, pinned: &str, wire: &[u8]) -> io::Result<Vec<u8>> {
        let stream = self.chain.dial(pinned, 443).await?;
        let name = ServerName::try_from(host.to_string())
            .map_err(|e| io::Error::other(format!("bad server name: {e}")))?;
        let mut tls = self.connector.connect(name, stream).await?;

        let mut request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/dns-message\r\n\
             Accept: application/dns-message\r\nContent-Length: {}\r\nConnection: close\r\n\
             User-Agent:\r\n\r\n",
            wire.len()
        )
        .into_bytes();
        request.extend_from_slice(wire);
        tls.write_all(&request).await?;
        tls.flush().await?;

        let mut raw = Vec::with_capacity(2048);
        let mut chunk = [0u8; 4096];
        loop {
            let n = tls.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
            if raw.len() > 256 * 1024 {
                return Err(io::Error::other("DoH response too large"));
            }
            if let Some(body) = split_http_body(&raw, false)? {
                return Ok(body);
            }
        }
        split_http_body(&raw, true)?.ok_or_else(|| io::Error::other("empty DoH response"))
    }

    pub async fn serve(self, listen: &str) -> io::Result<()> {
        let addr: SocketAddr = listen
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{e}")))?;
        let udp = Arc::new(UdpSocket::bind(addr).await?);
        let tcp = TcpListener::bind(addr).await?;
        println!("dns listening on {addr} (udp+tcp)");

        let udp_resolver = self.clone();
        let udp_sock = udp.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let (len, peer) = match udp_sock.recv_from(&mut buf).await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let query = buf[..len].to_vec();
                let resolver = udp_resolver.clone();
                let sock = udp_sock.clone();
                tokio::spawn(async move {
                    let response = resolver.resolve(&query).await;
                    if response.is_empty() {
                        return;
                    }
                    let udp_size = parse_query(&query).map(|q| q.udp_size).unwrap_or(512).clamp(512, 4096);
                    let payload = if response.len() > udp_size {
                        truncate(&response)
                    } else {
                        response
                    };
                    let _ = sock.send_to(&payload, peer).await;
                });
            }
        });

        loop {
            let (mut stream, _peer) = tcp.accept().await?;
            let resolver = self.clone();
            tokio::spawn(async move {
                loop {
                    let mut len_buf = [0u8; 2];
                    if stream.read_exact(&mut len_buf).await.is_err() {
                        break;
                    }
                    let len = u16::from_be_bytes(len_buf) as usize;
                    let mut wire = vec![0u8; len];
                    if stream.read_exact(&mut wire).await.is_err() {
                        break;
                    }
                    let response = resolver.resolve(&wire).await;
                    if response.is_empty() {
                        break;
                    }
                    if stream.write_all(&(response.len() as u16).to_be_bytes()).await.is_err() {
                        break;
                    }
                    if stream.write_all(&response).await.is_err() {
                        break;
                    }
                }
            });
        }
    }
}

fn split_http_body(raw: &[u8], eof: bool) -> io::Result<Option<Vec<u8>>> {
    let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(if eof { Some(Vec::new()) } else { None });
    };
    let head = String::from_utf8_lossy(&raw[..pos]).to_string();
    let body = &raw[pos + 4..];
    let mut status = 0u16;
    for (index, line) in head.lines().enumerate() {
        if index == 0 {
            status = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        }
    }
    if status != 200 {
        return Err(io::Error::other(format!("DoH HTTP {status}")));
    }
    let mut length: Option<usize> = None;
    let mut chunked = false;
    for line in head.lines().skip(1) {
        let lower = line.to_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().ok();
        }
        if lower.contains("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    if chunked {
        if let Some(body) = dechunk(body) {
            return Ok(Some(body));
        }
        return Ok(if eof { Some(Vec::new()) } else { None });
    }
    match length {
        Some(expected) if body.len() >= expected => Ok(Some(body[..expected].to_vec())),
        Some(_) => Ok(if eof { Some(body.to_vec()) } else { None }),
        None => Ok(if eof { Some(body.to_vec()) } else { None }),
    }
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let pos = body.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&body[..pos]).ok()?;
        let size_hex = size_line.split(';').next()?.trim();
        let size = usize::from_str_radix(size_hex, 16).ok()?;
        body = &body[pos + 2..];
        if size == 0 {
            return Some(out);
        }
        if body.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn truncate(response: &[u8]) -> Vec<u8> {
    if response.len() < 12 {
        return response.to_vec();
    }
    let flags = u16::from_be_bytes([response[2], response[3]]) | 0x0200; // TC
    let qdcount = u16::from_be_bytes([response[4], response[5]]);
    let mut offset = 12;
    for _ in 0..qdcount {
        match parse_name(response, offset) {
            Ok((_n, next)) => offset = next + 4,
            Err(_) => break,
        }
    }
    let mut out = Vec::with_capacity(offset);
    out.extend_from_slice(&response[..2]);
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&qdcount.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&response[12..offset.min(response.len())]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut wire = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        wire.extend_from_slice(&encode_name(name).unwrap());
        wire.extend_from_slice(&qtype.to_be_bytes());
        wire.extend_from_slice(&1u16.to_be_bytes());
        wire
    }

    #[test]
    fn roundtrips_query() {
        let wire = query("example.com", TYPE_A);
        let parsed = parse_query(&wire).unwrap();
        assert_eq!(parsed.name, "example.com");
        assert_eq!(parsed.qtype, TYPE_A);
        assert_eq!(parsed.id, 0x1234);
        assert_eq!(parsed.udp_size, 512);
    }

    #[test]
    fn builds_blocked_answer() {
        let parsed = parse_query(&query("ads.example", TYPE_A)).unwrap();
        let response = build_address_response(&parsed, "0.0.0.0".parse().unwrap(), 60);
        assert_eq!(rcode_of(&response), RCODE_NOERROR);
        assert_eq!(&response[0..2], &0x1234u16.to_be_bytes());
        assert_eq!(scan_ttl(&response), 60);
    }

    #[test]
    fn truncation_sets_tc() {
        let parsed = parse_query(&query("example.com", TYPE_A)).unwrap();
        let records: Vec<(String, u16, u32, Vec<u8>)> = (0..40)
            .map(|i| ("example.com".to_string(), TYPE_A, 30, vec![10, 0, 0, i as u8]))
            .collect();
        let big = build_response(&parsed, RCODE_NOERROR, &records);
        let small = truncate(&big);
        assert!(small.len() < big.len());
        assert_eq!(u16::from_be_bytes([small[2], small[3]]) & 0x0200, 0x0200);
        assert_eq!(parse_query(&small).unwrap().name, "example.com");
    }

    #[test]
    fn dechunks() {
        let body = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(dechunk(body).unwrap(), b"hello world");
    }

    #[test]
    fn splits_http_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let body = split_http_body(raw, false).unwrap().unwrap();
        assert_eq!(body, b"hello");
    }
}
