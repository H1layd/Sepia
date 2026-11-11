"""Linux backend: nftables killswitch + redirect, GNOME/systemd proxy plumbing."""
from __future__ import annotations

import re
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .. import util
from .base import (FirewallBackend, PlatformError, SystemProxy, have,
                   require_root, run, uniq)

LOG = util.get_logger("linux")

TABLE = "sepia"
RESOLV_CONF = Path("/etc/resolv.conf")
PROFILE_D = Path("/etc/profile.d/sepia-proxy.sh")


class NftFirewall(FirewallBackend):
    name = "nftables"
    supports_transparent = True
    supports_original_dst = True
    transparent_scope = "all-tcp"

    def __init__(self, run_dir: Path):
        self.run_dir = Path(run_dir)
        self.rules_path = self.run_dir / "nft-sepia.nft"

    def available(self) -> bool:
        return have("nft") and require_nft_version()

    def detect_interface(self) -> Optional[str]:
        _rc, out, _ = run(["ip", "route", "show", "default"], check=False)
        match = re.search(r"default\s+.*\s+dev\s+(\S+)", out)
        return match.group(1) if match else None

    # -- rules ------------------------------------------------------------- #

    def build_ruleset(self, cfg: Dict[str, Any], allowlist: List[Tuple[str, int]],
                      iface: Optional[str]) -> str:
        listen = cfg["listen"]
        redirect_port = util.split_hostport(str(listen["redirect"]))[1]
        dns_port = util.split_hostport(str(listen["dns"]))[1]
        fw = cfg["firewall"]
        allow_ips = sorted({ip for ip, _ in allowlist if util.ip_version(ip) == 4})
        hop_ports = sorted({port for _ip, port in allowlist})
        extra_ports = [int(p) for p in (fw.get("allow_ports") or [])]
        ports_all = sorted(set(hop_ports) | set(extra_ports))
        redirect_ports = sorted(set(fw.get("redirect_ports") or [80, 443]))
        bypass_uids = [int(u) for u in (fw.get("bypass_uids") or [])]
        lan_nets = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16"]

        parts: List[str] = [f"# sepia nftables ruleset generated {time.strftime('%Y-%m-%dT%H:%M:%S')}"]
        parts.append(f"table inet {TABLE} {{")

        if allow_ips:
            parts.append("  set hops {")
            parts.append("    type ipv4_addr; flags interval;")
            parts.append(f"    elements = {{ {', '.join(allow_ips)} }}")
            parts.append("  }")
        if bypass_uids:
            parts.append("  set bypass_uids {")
            parts.append("    type uid; flags interval;")
            parts.append(f"    elements = {{ {', '.join(str(u) for u in bypass_uids)} }}")
            parts.append("  }")
        if ports_all:
            parts.append("  set hop_ports {")
            parts.append("    type inet_service; flags interval;")
            parts.append(f"    elements = {{ {', '.join(str(p) for p in ports_all)} }}")
            parts.append("  }")

        if fw.get("transparent"):
            parts.append("  chain out_nat {")
            parts.append("    type nat hook output priority -100; policy accept;")
            parts.append("    ip daddr 127.0.0.0/8 return")
            parts.append("    meta mark 1 return")
            if bypass_uids:
                parts.append("    meta skuid @bypass_uids return")
            if allow_ips:
                parts.append("    ip daddr @hops tcp dport @hop_ports return")
            port_set = ", ".join(str(p) for p in redirect_ports)
            parts.append(
                f"    meta l4proto tcp tcp dport {{ {port_set} }} "
                f"counter redirect to :{redirect_port}"
            )
            if cfg["dns"].get("enabled"):
                parts.append(f"    udp dport 53 counter redirect to :{dns_port}")
                parts.append(f"    tcp dport 53 counter redirect to :{dns_port}")
            parts.append("  }")

        if fw.get("killswitch", True):
            parts.append("  chain out_filter {")
            parts.append("    type filter hook output priority filter; policy drop;")
            parts.append("    ct state established,related accept")
            parts.append("    oifname \"lo\" accept")
            parts.append("    ip daddr 127.0.0.0/8 accept")
            parts.append("    ip6 daddr ::1 accept")
            parts.append("    meta mark 1 accept")
            if bypass_uids:
                parts.append("    meta skuid @bypass_uids accept")
            if allow_ips:
                parts.append("    ip daddr @hops tcp dport @hop_ports accept")
            parts.append("    udp dport { 67, 68 } accept")
            if fw.get("allow_lan"):
                parts.append(f"    ip daddr {{ {', '.join(lan_nets)} }} accept")
            parts.append("    counter log prefix \"sepia-drop: \" drop")
            parts.append("  }")
        parts.append("}")
        return "\n".join(parts) + "\n"

    # -- apply / teardown -------------------------------------------------- #

    def apply(self, ruleset: str) -> None:
        require_root()
        self.run_dir.mkdir(parents=True, exist_ok=True)
        util.atomic_write(self.rules_path, ruleset)
        run(["nft", "delete", "table", "inet", TABLE], check=False)
        run(["nft", "-f", str(self.rules_path)])
        LOG.info("nftables ruleset installed (%s)", self.rules_path)

    def teardown(self) -> None:
        require_root()
        run(["nft", "delete", "table", "inet", TABLE], check=False)
        LOG.info("nftables table removed")

    def status(self) -> Dict[str, Any]:
        rc, out, _ = run(["nft", "list", "table", "inet", TABLE], check=False)
        return {
            "backend": self.name,
            "enabled": rc == 0,
            "rules_installed": rc == 0,
            "interface": self.detect_interface(),
            "rules_file": str(self.rules_path),
            "counters": _nft_counters(out) if rc == 0 else {},
        }

    def visible_rules(self) -> str:
        return run(["nft", "list", "table", "inet", TABLE], check=False)[1]


def require_nft_version() -> bool:
    rc, _out, _err = run(["nft", "--version"], check=False)
    return rc == 0


def _nft_counters(text: str) -> Dict[str, int]:
    out: Dict[str, int] = {}
    for line in text.splitlines():
        if "counter" not in line or "redirect" not in line:
            continue
        match = re.search(r"dport \S+ .*?counter packets (\d+)", line)
        if match:
            out[line.strip()[:60]] = int(match.group(1))
    return out


class LinuxSystemProxy(SystemProxy):
    name = "gsettings/systemd-resolved"

    def available(self) -> bool:
        return have("gsettings") or have("resolvectl") or True

    def __init__(self, run_dir: Path):
        self.run_dir = Path(run_dir)
        self.backup_path = self.run_dir / "linux-system-proxy.json"

    # -- helpers ----------------------------------------------------------- #

    def _iface(self) -> Optional[str]:
        _rc, out, _ = run(["ip", "route", "show", "default"], check=False)
        match = re.search(r"\sdev\s+(\S+)", out)
        return match.group(1) if match else None

    def _gsettings_get(self, schema: str, key: str) -> Optional[str]:
        rc, out, _ = run(["gsettings", "get", schema, key], check=False)
        return out.strip() if rc == 0 else None

    # -- snapshot / apply -------------------------------------------------- #

    def snapshot(self) -> Dict[str, Any]:
        data: Dict[str, Any] = {"iface": self._iface(), "files": {}}
        if have("gsettings"):
            for schema, key in (
                ("org.gnome.system.proxy", "mode"),
                ("org.gnome.system.proxy.http", "host"),
                ("org.gnome.system.proxy.http", "port"),
                ("org.gnome.system.proxy.https", "host"),
                ("org.gnome.system.proxy.https", "port"),
                ("org.gnome.system.proxy.socks", "host"),
                ("org.gnome.system.proxy.socks", "port"),
                ("org.gnome.system.proxy", "ignore-hosts"),
            ):
                data[f"gsettings:{schema}::{key}"] = self._gsettings_get(schema, key)
        for path in (RESOLV_CONF, PROFILE_D):
            if path.exists():
                data["files"][str(path)] = path.read_text(encoding="utf-8", errors="replace")
        util.write_json(self.backup_path, data)
        util.secure_file(self.backup_path)
        return data

    def apply(self, http_host: str, http_port: int, socks_host: str, socks_port: int,
              dns_host: Optional[str] = None, dns_port: Optional[int] = None,
              bypass: Optional[List[str]] = None) -> None:
        if have("gsettings") and self._gsettings_get("org.gnome.system.proxy", "mode") is not None:
            run(["gsettings", "set", "org.gnome.system.proxy", "mode", "manual"], check=False)
            for schema, port in (
                ("org.gnome.system.proxy.http", http_port),
                ("org.gnome.system.proxy.https", http_port),
            ):
                run(["gsettings", "set", schema, "host", http_host], check=False)
                run(["gsettings", "set", schema, "port", str(port)], check=False)
            run(["gsettings", "set", "org.gnome.system.proxy.socks", "host", socks_host],
                check=False)
            run(["gsettings", "set", "org.gnome.system.proxy.socks", "port", str(socks_port)],
                check=False)
            ignore = bypass or ["localhost", "127.0.0.0/8", "::1"]
            run(["gsettings", "set", "org.gnome.system.proxy", "ignore-hosts",
                 str(ignore)], check=False)

        # Shell environment for CLI tools that honour *_proxy.
        env_lines = [
            "# managed by sepia; removed by 'sepia down'",
            f'export http_proxy="http://{http_host}:{http_port}"',
            f'export https_proxy="http://{http_host}:{http_port}"',
            f'export HTTP_PROXY="http://{http_host}:{http_port}"',
            f'export HTTPS_PROXY="http://{http_host}:{http_port}"',
            f'export ALL_PROXY="socks5h://{socks_host}:{socks_port}"',
            'export no_proxy="localhost,127.0.0.1,::1"',
        ]
        try:
            util.atomic_write(PROFILE_D, "\n".join(env_lines) + "\n", mode=0o644)
        except OSError as exc:
            LOG.warning("could not write %s: %s", PROFILE_D, exc)

        if dns_host:
            iface = self._iface()
            if have("resolvectl") and iface:
                run(["resolvectl", "dns", iface, dns_host], check=False)
                run(["resolvectl", "domain", iface, "~."], check=False)
                run(["resolvectl", "flush-caches"], check=False)
            else:
                util.atomic_write(
                    RESOLV_CONF,
                    f"# managed by sepia\nnameserver {dns_host}\noptions edns0 trust-ad\n",
                )
        LOG.info("system proxy applied")

    def restore(self, snapshot: Dict[str, Any]) -> None:
        for key, value in (snapshot or {}).items():
            if not key.startswith("gsettings:") or value is None:
                continue
            schema, _, rest = key[len("gsettings:"):].partition("::")
            run(["gsettings", "set", schema, rest, value], check=False)
        files = (snapshot or {}).get("files") or {}
        for path_s, content in files.items():
            path = Path(path_s)
            try:
                util.atomic_write(path, content, mode=0o644)
            except OSError as exc:
                LOG.warning("could not restore %s: %s", path, exc)
        if str(PROFILE_D) not in files and PROFILE_D.exists():
            try:
                PROFILE_D.unlink()
            except OSError:
                pass
        if have("resolvectl"):
            run(["resolvectl", "flush-caches"], check=False)
        LOG.info("system proxy restored")
