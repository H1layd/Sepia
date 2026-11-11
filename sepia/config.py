"""Configuration model, defaults and validation."""
from __future__ import annotations

import copy
import json
import shutil
from pathlib import Path
from typing import Any, Dict, List, Optional

from . import util

DEFAULT_DOH = [
    {
        "url": "https://cloudflare-dns.com/dns-query",
        "bootstraps": ["1.1.1.1", "1.0.0.1", "104.16.248.249", "104.16.249.249"],
    },
    {
        "url": "https://dns.quad9.net/dns-query",
        "bootstraps": ["9.9.9.9", "149.112.112.112"],
    },
    {
        "url": "https://dns.google/dns-query",
        "bootstraps": ["8.8.8.8", "8.8.4.4"],
    },
]

DEFAULT_CONFIG: Dict[str, Any] = {
    "version": 1,
    "listen": {
        "socks": "127.0.0.1:10800",
        "http": "127.0.0.1:18080",
        "redirect": "127.0.0.1:18081",
        "dns": "127.0.0.1:15353",
        "control": "127.0.0.1:19099",
    },
    "chain": {
        # Empty = direct. Each hop is dialled through the previous one.
        #   {"type": "socks5", "addr": "127.0.0.1:9050"}
        #   {"type": "socks5", "addr": "10.0.0.5:1080", "username": "u", "password": "p"}
        #   {"type": "http",   "addr": "proxy.example.net:3128", "username": "u", "password": "p"}
        "hops": [],
        "connect_timeout": 12.0,
        "health_url": "https://1.1.1.1/cdn-cgi/trace",
        "health_interval": 0,
        "on_hop_failure": "fail",  # fail | skip
    },
    "dns": {
        "enabled": True,
        "servers": DEFAULT_DOH,
        "cache_ttl_max": 3600,
        "cache_negative_ttl": 30,
        "block": [],
        "block_response": "0.0.0.0",
        "passthrough_local": False,
    },
    "rules": [],
    "firewall": {
        "enabled": True,
        "killswitch": True,
        "transparent": "auto",
        "iface": "auto",
        "auto_iface": True,
        "redirect_ports": [80, 443],
        "block_ipv6": True,
        "allow_lan": False,
        "allow_ports": [],
        "bypass_uids": [],
    },
    "system_proxy": {"enabled": False},
    "verify": {
        "ip_endpoints": [
            "https://cloudflare.com/cdn-cgi/trace",
            "https://api.ipify.org?format=json",
            "https://ifconfig.co/json",
        ],
        "dns_leak_endpoint": "https://edns.ip-api.com/json",
        "header_endpoint": "https://httpbin.org/headers",
        "timeout": 12.0,
    },
    "paths": {},
}


LEGACY_DIR = Path.home() / ".veil"


def migrate_legacy() -> bool:
    """Adopt a pre-rename ~/.veil directory so nothing is lost on upgrade."""
    target = Path.home() / ".sepia"
    if target.exists() or not LEGACY_DIR.exists():
        return False
    import shutil

    try:
        shutil.copytree(LEGACY_DIR, target)
        marker = target / "MIGRATED_FROM_VEIL"
        marker.write_text(
            "This directory was copied from ~/.veil when the project was renamed to Sepia.\n"
            "The old directory is left untouched; remove it once you are satisfied.\n"
        )
        return True
    except OSError:
        return False


def config_dir() -> Path:
    return Path.home() / ".sepia"


def config_path() -> Path:
    return config_dir() / "config.json"


def run_dir() -> Path:
    return config_dir() / "run"


def log_dir() -> Path:
    return config_dir() / "logs"


def instance_id(cfg: Dict[str, Any]) -> str:
    """Two sepia instances may run side by side (e.g. one as a hop), so every
    runtime artefact is namespaced by the control port."""
    try:
        return str(util.split_hostport(str(cfg["listen"]["control"]))[1])
    except Exception:
        return "default"


def all_paths() -> Dict[str, str]:
    return {
        "config": str(config_path()),
        "run": str(run_dir()),
        "logs": str(log_dir()),
    }


def deep_merge(base: Dict[str, Any], over: Dict[str, Any]) -> Dict[str, Any]:
    out = copy.deepcopy(base)
    for key, value in (over or {}).items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = deep_merge(out[key], value)
        else:
            out[key] = copy.deepcopy(value)
    return out


class ConfigError(ValueError):
    pass


def normalise(raw: Dict[str, Any]) -> Dict[str, Any]:
    """Fill defaults and coerce types. Raises ConfigError on hard problems."""
    cfg = deep_merge(DEFAULT_CONFIG, raw)
    cfg["paths"] = all_paths()

    listen = cfg["listen"]
    for key in ("socks", "http", "redirect", "dns", "control"):
        host, port = util.split_hostport(str(listen[key]), 0)
        if not host or not (0 < port < 65536):
            raise ConfigError(f"listen.{key}: bad address {listen[key]!r}")
        listen[key] = util.join_hostport(host, port)

    chain = cfg["chain"]
    hops: List[Dict[str, Any]] = []
    for index, hop in enumerate(chain.get("hops") or []):
        if not isinstance(hop, dict) or "addr" not in hop:
            raise ConfigError(f"chain.hops[{index}]: expected object with 'addr'")
        kind = str(hop.get("type", "socks5")).lower()
        if kind not in ("socks5", "socks5h", "http"):
            raise ConfigError(f"chain.hops[{index}]: unsupported type {kind!r}")
        host, port = util.split_hostport(str(hop["addr"]), 1080 if kind.startswith("socks") else 3128)
        if not host or not (0 < port < 65536):
            raise ConfigError(f"chain.hops[{index}]: bad address {hop['addr']!r}")
        entry = {
            "type": "http" if kind == "http" else "socks5",
            "addr": util.join_hostport(host, port),
            "username": hop.get("username"),
            "password": hop.get("password"),
            "label": hop.get("label") or f"hop{index + 1}",
        }
        hops.append(entry)
    chain["hops"] = hops
    chain["connect_timeout"] = float(chain.get("connect_timeout") or 12.0)
    if chain.get("on_hop_failure") not in ("fail", "skip"):
        chain["on_hop_failure"] = "fail"

    dns = cfg["dns"]
    servers = []
    for server in dns.get("servers") or []:
        if isinstance(server, str):
            server = {"url": server, "bootstraps": []}
        if not isinstance(server, dict) or not str(server.get("url", "")).startswith("https://"):
            raise ConfigError("dns.servers: only https:// DoH endpoints are supported")
        servers.append(
            {
                "url": server["url"].rstrip("/"),
                "bootstraps": [str(b) for b in (server.get("bootstraps") or [])],
            }
        )
    if not servers:
        servers = copy.deepcopy(DEFAULT_DOH)
    dns["servers"] = servers
    dns["cache_ttl_max"] = int(dns.get("cache_ttl_max") or 3600)
    dns["block"] = [str(d).lower().strip(".") for d in (dns.get("block") or [])]
    if not util.is_ip(str(dns.get("block_response", ""))):
        dns["block_response"] = "0.0.0.0"

    rules = []
    for rule in cfg.get("rules") or []:
        if not isinstance(rule, dict) or "domain" not in rule:
            raise ConfigError("rules: each rule needs 'domain'")
        action = str(rule.get("action", "chain")).lower()
        if action not in ("chain", "direct", "block"):
            raise ConfigError(f"rules: bad action {action!r}")
        rules.append({"domain": str(rule["domain"]).lower().strip("."), "action": action})
    cfg["rules"] = rules

    fw = cfg["firewall"]
    if isinstance(fw.get("transparent"), str) and fw["transparent"] != "auto":
        fw["transparent"] = fw["transparent"].lower() in ("1", "true", "yes", "on")
    fw["bypass_uids"] = sorted({int(u) for u in fw.get("bypass_uids") or []})
    fw["redirect_ports"] = sorted({int(p) for p in fw.get("redirect_ports") or [80, 443]})
    fw["allow_ports"] = sorted({int(p) for p in fw.get("allow_ports") or []})
    return cfg


def load(path: Optional[Path] = None) -> Dict[str, Any]:
    if path is None:
        migrate_legacy()
    path = Path(path) if path else config_path()
    if not path.exists():
        raise ConfigError(f"config not found: {path} (run: sepia init)")
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ConfigError(f"{path}: invalid JSON: {exc}") from exc
    if not isinstance(raw, dict):
        raise ConfigError(f"{path}: expected a JSON object")
    return normalise(raw)


def save(cfg: Dict[str, Any], path: Optional[Path] = None) -> Path:
    path = Path(path) if path else config_path()
    payload = {k: v for k, v in cfg.items() if k != "paths"}
    util.write_json(path, payload)
    util.secure_file(path)
    return path


def init(force: bool = False) -> Path:
    migrate_legacy()
    path = config_path()
    if path.exists() and not force:
        raise ConfigError(f"config already exists: {path} (use --force to overwrite)")
    for directory in (config_dir(), run_dir(), log_dir()):
        directory.mkdir(parents=True, exist_ok=True)
    if path.exists():
        backup = path.with_suffix(f".json.bak-{util.rand_hex(3)}")
        shutil.copy2(path, backup)
    util.write_json(path, DEFAULT_CONFIG)
    util.secure_file(path)
    return path
