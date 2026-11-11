//! Configuration: same JSON schema as the Python daemon, only the fields the
//! data plane needs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Listen {
    #[serde(default = "default_socks")]
    pub socks: String,
    #[serde(default = "default_http")]
    pub http: String,
    #[serde(default = "default_control")]
    pub control: String,
    /// Python's config keeps the resolver address here; rust follows it unless
    /// dns.listen was set explicitly.
    #[serde(default)]
    pub dns: Option<String>,
}

fn default_socks() -> String { "127.0.0.1:10800".into() }
fn default_http() -> String { "127.0.0.1:18080".into() }
fn default_control() -> String { "127.0.0.1:19099".into() }

impl Default for Listen {
    fn default() -> Self {
        Listen {
            socks: default_socks(),
            http: default_http(),
            control: default_control(),
            dns: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hop {
    #[serde(rename = "type", default = "default_hop_type")]
    pub kind: String,
    pub addr: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

fn default_hop_type() -> String { "socks5".into() }

impl Hop {
    pub fn parse_addr(&self) -> Result<(String, u16), String> {
        split_hostport(&self.addr).ok_or_else(|| format!("bad hop address: {}", self.addr))
    }
    pub fn label(&self) -> String {
        self.label.clone().unwrap_or_else(|| self.addr.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainCfg {
    #[serde(default)]
    pub hops: Vec<Hop>,
    #[serde(default = "default_timeout")]
    pub connect_timeout: f64,
    #[serde(default = "default_failure")]
    pub on_hop_failure: String,
}

fn default_timeout() -> f64 { 12.0 }
fn default_failure() -> String { "fail".into() }

impl Default for ChainCfg {
    fn default() -> Self {
        ChainCfg { hops: Vec::new(), connect_timeout: 12.0, on_hop_failure: "fail".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub domain: String,
    #[serde(default = "default_action")]
    pub action: String,
}

fn default_action() -> String { "chain".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsServer {
    pub url: String,
    #[serde(default)]
    pub bootstraps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsCfg {
    #[serde(default)]
    pub enabled: bool,
    // Every field needs an explicit default: a bare #[serde(default)] would give
    // an empty string for listen and the resolver would fail to bind.
    #[serde(default = "default_dns_listen")]
    pub listen: String,
    #[serde(default)]
    pub servers: Vec<DnsServer>,
    #[serde(default = "default_ttl")]
    pub cache_ttl_max: u64,
    #[serde(default)]
    pub block: Vec<String>,
    #[serde(default = "default_block_response")]
    pub block_response: String,
}

fn default_dns_listen() -> String { "127.0.0.1:15353".into() }

fn default_ttl() -> u64 { 3600 }
fn default_block_response() -> String { "0.0.0.0".into() }

impl Default for DnsCfg {
    fn default() -> Self {
        DnsCfg {
            enabled: false,
            listen: "127.0.0.1:15353".into(),
            servers: Vec::new(),
            cache_ttl_max: 3600,
            block: Vec::new(),
            block_response: "0.0.0.0".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub listen: Listen,
    #[serde(default)]
    pub chain: ChainCfg,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub dns: DnsCfg,
}

impl Config {
    pub fn load(path: &str) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let mut cfg: Config = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        if let Some(listen_dns) = cfg.listen.dns.clone() {
            let is_default = cfg.dns.listen == "127.0.0.1:15353";
            if is_default && !listen_dns.is_empty() {
                cfg.dns.listen = listen_dns;
            }
        }
        Ok(cfg)
    }
}

pub fn split_hostport(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.trim_start_matches(':').parse().ok()?;
        return Some((host.to_string(), port));
    }
    let (host, port) = value.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(split_hostport("127.0.0.1:1080"), Some(("127.0.0.1".into(), 1080)));
        assert_eq!(split_hostport("[::1]:9050"), Some(("::1".into(), 9050)));
        assert_eq!(split_hostport("nonsense"), None);
        assert_eq!(split_hostport("host:99999"), None);
    }

    #[test]
    fn dns_defaults_are_addresses() {
        let cfg: Config = serde_json::from_str(r#"{"dns":{"enabled":true}}"#).unwrap();
        assert_eq!(cfg.dns.listen, "127.0.0.1:15353");
        assert!(split_hostport(&cfg.dns.listen).is_some());
    }

    #[test]
    fn dns_listen_follows_the_python_field() {
        let dir = std::env::temp_dir().join(format!("sepia-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        std::fs::write(&path, r#"{"listen":{"dns":"127.0.0.1:61553"},"dns":{"enabled":true}}"#).unwrap();
        let cfg = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(cfg.dns.listen, "127.0.0.1:61553");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_are_sane() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.listen.socks, "127.0.0.1:10800");
        assert_eq!(cfg.chain.connect_timeout, 12.0);
        assert!(cfg.chain.hops.is_empty());
    }

    #[test]
    fn parses_hops() {
        let cfg: Config = serde_json::from_str(
            r#"{"chain":{"hops":[{"type":"socks5","addr":"127.0.0.1:9050","username":"u","password":"p"}]}}"#,
        )
        .unwrap();
        assert_eq!(cfg.chain.hops.len(), 1);
        assert_eq!(cfg.chain.hops[0].parse_addr().unwrap(), ("127.0.0.1".into(), 9050));
        assert_eq!(cfg.chain.hops[0].username.as_deref(), Some("u"));
    }
}
