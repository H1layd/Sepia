//! sepia-core: the data plane.
//!
//! Same config file as the Python daemon; serves the SOCKS5 and HTTP endpoints
//! plus the DoH resolver, and exposes the same /status shape on the control
//! port so the CLI cannot tell the engines apart.

mod chain;
mod config;
mod dns;
mod proxy;
mod rules;
mod socks5;

use std::process::ExitCode;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{split_hostport, Config};
use crate::dns::Resolver;
use crate::proxy::{serve_http, serve_socks, Shared, Stats};
use crate::rules::Router;

fn usage() -> &'static str {
    "sepia-core — data plane\n\n\
     usage: sepia-core [--config PATH] [--print-config] [--version]\n\n\
     engines: the Python daemon can supervise this binary with `sepia up --engine rust`.\n"
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path = default_config_path();
    let mut print_config = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                index += 1;
                if index >= args.len() {
                    eprintln!("error: --config needs a path");
                    return ExitCode::from(2);
                }
                config_path = args[index].clone();
            }
            "--print-config" => print_config = true,
            "--version" | "-V" => {
                println!("sepia-core {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("error: unknown argument {other}\n\n{}", usage());
                return ExitCode::from(2);
            }
        }
        index += 1;
    }

    let cfg = match Config::load(&config_path) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(1);
        }
    };
    if print_config {
        match serde_json::to_string_pretty(&cfg) {
            Ok(text) => println!("{text}"),
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::from(1);
            }
        }
        return ExitCode::SUCCESS;
    }

    run(cfg).await
}

fn default_config_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/.sepia/config.json")
}

async fn run(cfg: Config) -> ExitCode {
    let chain = chain::Chain::new(&cfg.chain);
    let router = Router::new(&cfg.rules);
    let stats = Stats::new();
    let shared = Arc::new(Shared { chain: chain.clone(), router, stats: stats.clone(), socks_auth: None });

    let socks_addr = match split_hostport(&cfg.listen.socks) {
        Some(addr) => addr,
        None => {
            eprintln!("error: bad listen.socks: {}", cfg.listen.socks);
            return ExitCode::from(1);
        }
    };
    let http_addr = match split_hostport(&cfg.listen.http) {
        Some(addr) => addr,
        None => {
            eprintln!("error: bad listen.http: {}", cfg.listen.http);
            return ExitCode::from(1);
        }
    };

    let socks_listener = match TcpListener::bind((socks_addr.0.as_str(), socks_addr.1)).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("error: cannot bind {}: {err}", cfg.listen.socks);
            return ExitCode::from(1);
        }
    };
    let http_listener = match TcpListener::bind((http_addr.0.as_str(), http_addr.1)).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("error: cannot bind {}: {err}", cfg.listen.http);
            return ExitCode::from(1);
        }
    };
    println!("SOCKS5 on {}", cfg.listen.socks);
    println!("HTTP   on {}", cfg.listen.http);
    println!("chain  {} hop(s)", chain.depth());
    for hop in chain.describe() {
        println!("       - {hop}");
    }

    tokio::spawn(serve_socks(socks_listener, shared.clone()));
    tokio::spawn(serve_http(http_listener, shared.clone()));

    if cfg.dns.enabled && !cfg.dns.servers.is_empty() {
        let resolver = Resolver::new(&cfg.dns, chain.clone());
        let listen = cfg.dns.listen.clone();
        tokio::spawn(async move {
            if let Err(err) = resolver.serve(&listen).await {
                eprintln!("dns error: {err}");
            }
        });
    }

    let control = Arc::new(Control {
        shared: shared.clone(),
        chain: chain.clone(),
        socks: cfg.listen.socks.clone(),
        http: cfg.listen.http.clone(),
    });
    tokio::spawn(control.serve(cfg.listen.control.clone()));

    match tokio::signal::ctrl_c().await {
        Ok(()) => {}
        Err(err) => eprintln!("signal error: {err}"),
    }
    println!("shutting down");
    ExitCode::SUCCESS
}

struct Control {
    shared: Arc<Shared>,
    chain: chain::Chain,
    socks: String,
    http: String,
}

impl Control {
    async fn serve(self: Arc<Self>, listen: String) {
        let Some((host, port)) = split_hostport(&listen) else {
            eprintln!("error: bad listen.control: {listen}");
            return;
        };
        let listener = match TcpListener::bind((host.as_str(), port)).await {
            Ok(l) => l,
            Err(err) => {
                eprintln!("error: cannot bind control {listen}: {err}");
                return;
            }
        };
        println!("control on {listen}");
        loop {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                break;
            };
            let this = self.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let body = match path.split('?').next().unwrap_or("/") {
                    "/health" => "{\"ok\":true,\"engine\":\"rust\"}".to_string(),
                    "/metrics" => this.shared.stats.snapshot().await.to_string(),
                    "/status" | "/" => {
                        let counters = this.shared.stats.snapshot().await;
                        serde_json::json!({
                            "engine": "rust",
                            "pid": std::process::id(),
                            "chain_depth": this.chain.depth(),
                            "hops": this.chain.describe(),
                            "listen": {"socks": this.socks, "http": this.http},
                            "counters": counters,
                        })
                        .to_string()
                    }
                    _ => "{\"error\":\"unknown endpoint\"}".to_string(),
                };
                let status = if body.starts_with("{\"error\"") {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    }
}
