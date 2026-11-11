//! SOCKS5 protocol: both directions.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const CMD_CONNECT: u8 = 0x01;
pub const ATYP_V4: u8 = 0x01;
pub const ATYP_DOMAIN: u8 = 0x03;
pub const ATYP_V6: u8 = 0x04;

#[derive(Debug)]
pub enum SocksError {
    Io(io::Error),
    Reply(u8),
    Protocol(String),
}

impl std::fmt::Display for SocksError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SocksError::Io(e) => write!(f, "io: {e}"),
            SocksError::Reply(code) => write!(f, "socks reply {code}: {}", reply_text(*code)),
            SocksError::Protocol(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<io::Error> for SocksError {
    fn from(e: io::Error) -> Self {
        SocksError::Io(e)
    }
}

pub fn reply_text(code: u8) -> &'static str {
    match code {
        0x00 => "succeeded",
        0x01 => "general failure",
        0x02 => "not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "ttl expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown",
    }
}

pub fn encode_address(host: &str, port: u16) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1 + 1 + host.len() + 2);
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        out.push(ATYP_V4);
        out.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        out.push(ATYP_V6);
        out.extend_from_slice(&ip.octets());
    } else {
        let bytes = host.as_bytes();
        if bytes.len() > 255 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "hostname too long"));
        }
        out.push(ATYP_DOMAIN);
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&port.to_be_bytes());
    Ok(out)
}

pub fn build_reply(code: u8) -> Vec<u8> {
    let mut out = vec![0x05, code, 0x00, ATYP_V4, 0, 0, 0, 0];
    out.extend_from_slice(&0u16.to_be_bytes());
    out
}

async fn read_address(stream: &mut TcpStream) -> io::Result<(String, u16)> {
    let atyp = stream.read_u8().await?;
    let host = match atyp {
        ATYP_V4 => {
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await?;
            std::net::Ipv4Addr::from(buf).to_string()
        }
        ATYP_V6 => {
            let mut buf = [0u8; 16];
            stream.read_exact(&mut buf).await?;
            std::net::Ipv6Addr::from(buf).to_string()
        }
        ATYP_DOMAIN => {
            let len = stream.read_u8().await? as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await?;
            String::from_utf8_lossy(&buf).into_owned()
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad address type {other}"),
            ))
        }
    };
    let port = stream.read_u16().await?;
    Ok((host, port))
}

/// Client side: reach `host:port` through the already-connected hop.
pub async fn client_connect(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(), SocksError> {
    let methods: Vec<u8> = if username.is_some() { vec![0x02] } else { vec![0x00] };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(&methods);
    stream.write_all(&greeting).await?;

    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(SocksError::Protocol("hop did not speak SOCKS5".into()));
    }
    match head[1] {
        0x02 => {
            let user = username.unwrap_or("");
            let pass = password.unwrap_or("");
            let mut auth = vec![0x01, user.len() as u8];
            auth.extend_from_slice(user.as_bytes());
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass.as_bytes());
            stream.write_all(&auth).await?;
            let mut reply = [0u8; 2];
            stream.read_exact(&mut reply).await?;
            if reply[1] != 0x00 {
                return Err(SocksError::Reply(0x02));
            }
        }
        0x00 => {}
        _ => return Err(SocksError::Reply(0x02)),
    }

    let mut request = vec![0x05, CMD_CONNECT, 0x00];
    request.extend_from_slice(&encode_address(host, port)?);
    stream.write_all(&request).await?;

    let mut head = [0u8; 3];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(SocksError::Protocol("bad SOCKS5 reply version".into()));
    }
    let _ = read_address(stream).await?;
    if head[1] != 0x00 {
        return Err(SocksError::Reply(head[1]));
    }
    Ok(())
}

/// Server side: negotiate and return the requested destination.
pub async fn server_handshake(
    stream: &mut TcpStream,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<(String, u16, u8), SocksError> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Err(SocksError::Protocol("not SOCKS5".into()));
    }
    let count = head[1] as usize;
    let mut methods = vec![0u8; count];
    stream.read_exact(&mut methods).await?;

    let want: u8 = if username.is_some() { 0x02 } else { 0x00 };
    if !methods.contains(&want) {
        stream.write_all(&[0x05, 0xFF]).await?;
        return Err(SocksError::Reply(0x02));
    }
    stream.write_all(&[0x05, want]).await?;

    if want == 0x02 {
        let ver = stream.read_u8().await?;
        if ver != 0x01 {
            return Err(SocksError::Protocol("bad auth version".into()));
        }
        let ulen = stream.read_u8().await? as usize;
        let mut user = vec![0u8; ulen];
        stream.read_exact(&mut user).await?;
        let plen = stream.read_u8().await? as usize;
        let mut pass = vec![0u8; plen];
        stream.read_exact(&mut pass).await?;
        let ok = String::from_utf8_lossy(&user) == username.unwrap_or("")
            && String::from_utf8_lossy(&pass) == password.unwrap_or("");
        stream
            .write_all(&[0x01, if ok { 0x00 } else { 0x01 }])
            .await?;
        if !ok {
            return Err(SocksError::Reply(0x02));
        }
    }

    let mut req = [0u8; 3];
    stream.read_exact(&mut req).await?;
    if req[0] != 0x05 {
        return Err(SocksError::Protocol("bad request version".into()));
    }
    let (host, port) = read_address(stream).await?;
    Ok((host, port, req[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_ipv4() {
        let bytes = encode_address("1.2.3.4", 443).unwrap();
        assert_eq!(bytes, vec![0x01, 1, 2, 3, 4, 0x01, 0xBB]);
    }

    #[test]
    fn encodes_domain() {
        let bytes = encode_address("example.com", 80).unwrap();
        assert_eq!(bytes[0], ATYP_DOMAIN);
        assert_eq!(bytes[1], 11);
        assert_eq!(&bytes[2..13], b"example.com");
        assert_eq!(&bytes[13..15], &80u16.to_be_bytes());
    }

    #[test]
    fn encodes_ipv6() {
        let bytes = encode_address("::1", 1080).unwrap();
        assert_eq!(bytes[0], ATYP_V6);
        assert_eq!(bytes.len(), 1 + 16 + 2);
    }

    #[test]
    fn reply_shape() {
        let reply = build_reply(0x05);
        assert_eq!(reply[0], 0x05);
        assert_eq!(reply[1], 0x05);
    }
}
