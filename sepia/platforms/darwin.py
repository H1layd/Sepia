"""macOS backend: PF ruleset + networksetup system proxy."""
from __future__ import annotations

import re
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .. import util
from .base import (CommandError, FirewallBackend, PlatformError, SystemProxy,
                   have, require_root, run)

LOG = util.get_logger("darwin")

ANCHOR_MARK = "# sepia-killswitch"


class PfFirewall(FirewallBackend):
    name = "pf (macOS)"
    supports_transparent = True
    supports_original_dst = False
    transparent_scope = "http"  # pf rdr loses the original destination on Darwin

    def __init__(self, run_dir: Path):
        self.run_dir = Path(run_dir)
        self.rules_path = self.run_dir / "pf-sepia.conf"
        self.snapshot_path = self.run_dir / "pf-snapshot.conf"
        self.meta_path = self.run_dir / "pf-meta.json"

    def available(self) -> bool:
        return have("pfctl")

    def detect_interface(self) -> Optional[str]:
        rc, out, _ = run(["route", "-n", "get", "default"], check=False)
        match = re.search(r"interface:\s*(\S+)", out)
        return match.group(1) if match else None

    # -- rules ------------------------------------------------------------- #

    def build_ruleset(self, cfg: Dict[str, Any], allowlist: List[Tuple[str, int]],
                      iface: Optional[str]) -> str:
        listen = cfg["listen"]
        redirect_port = util.split_hostport(str(listen["redirect"]))[1]
        dns_port = util.split_hostport(str(listen["dns"]))[1]
        fw = cfg["firewall"]
        iface = iface or "en0"
        allow_ips = sorted({ip for ip, _port in allowlist})
        hop_ports = sorted({port for _ip, port in allowlist})
        extra_ports = list(fw.get("allow_ports") or [])

        lines: List[str] = [
            f"{ANCHOR_MARK} generated {time.strftime('%Y-%m-%dT%H:%M:%S')}",
            "set skip on lo0",
            "scrub in all fragment reassemble",
        ]
        if allow_ips:
            lines.append(f"table <sepia_hops> persist {{ {', '.join(allow_ips)} }}")
        else:
            lines.append("table <sepia_hops> persist { }")

        if fw.get("transparent"):
            ports = " ".join(str(p) for p in sorted(set(fw.get("redirect_ports") or [80, 443])))
            target = f"127.0.0.1 port {redirect_port}"
            not_hops = "to ! <sepia_hops> " if allow_ips else "to any "
            lines.append(
                f"rdr pass on {iface} inet proto tcp from any {not_hops}"
                f"port {{ {ports} }} -> {target}"
            )
            if cfg["dns"].get("enabled"):
                lines.append(
                    f"rdr pass on {iface} inet proto udp from any to any port 53 "
                    f"-> 127.0.0.1 port {dns_port}"
                )
                lines.append(
                    f"rdr pass on {iface} inet proto tcp from any to any port 53 "
                    f"-> 127.0.0.1 port {dns_port}"
                )

        if fw.get("killswitch", True):
            lines.append("pass out quick on lo0 all")
            if allow_ips:
                port_clause = (
                    f" port {{ {', '.join(str(p) for p in hop_ports)} }}" if hop_ports else ""
                )
                lines.append(
                    f"pass out quick inet proto tcp from any to <sepia_hops>{port_clause}"
                )
                if extra_ports:
                    lines.append(
                        "pass out quick inet proto tcp from any to <sepia_hops> "
                        f"port {{ {', '.join(str(p) for p in extra_ports)} }}"
                    )
            lines.append("pass out quick inet proto udp from any to any port 67")
            lines.append("pass out quick inet proto udp from any to any port 68")
            if fw.get("block_ipv6", True):
                lines.append("block drop out quick inet6 all")
            lines.append("block drop out quick all")
        return "\n".join(lines) + "\n"

    # -- apply / teardown -------------------------------------------------- #

    def apply(self, ruleset: str) -> None:
        require_root()
        self.run_dir.mkdir(parents=True, exist_ok=True)
        was_enabled = self._enabled()
        if not self.snapshot_path.exists():
            nat = run(["pfctl", "-s", "nat"], check=False)[1]
            rules = run(["pfctl", "-s", "rules"], check=False)[1]
            snapshot = f"# sepia snapshot {time.strftime('%Y-%m-%dT%H:%M:%S')}\n"
            if nat.strip():
                snapshot += nat.rstrip() + "\n"
            if rules.strip():
                snapshot += rules.rstrip() + "\n"
            util.atomic_write(self.snapshot_path, snapshot)
            util.write_json(self.meta_path, {"was_enabled": was_enabled,
                                             "applied_at": int(time.time())})
        util.atomic_write(self.rules_path, ruleset)
        run(["pfctl", "-f", str(self.rules_path)])
        if not was_enabled:
            rc, _out, err = run(["pfctl", "-E"], check=False)
            if rc != 0 and "already" not in err.lower():
                raise PlatformError(f"pfctl -E failed: {err.strip()}")
        LOG.info("pf ruleset installed (%s)", self.rules_path)

    def teardown(self) -> None:
        require_root()
        meta = util.read_json(self.meta_path)
        restored = False
        if self.snapshot_path.exists():
            text = self.snapshot_path.read_text(encoding="utf-8")
            body = "\n".join(
                line for line in text.splitlines() if not line.startswith("# sepia snapshot")
            ).strip()
            if body:
                run(["pfctl", "-f", "-"], stdin=body + "\n", check=False)
            else:
                run(["pfctl", "-f", "/etc/pf.conf"], check=False)
            restored = True
            LOG.info("previous pf ruleset restored")
        if not restored:
            run(["pfctl", "-f", "/etc/pf.conf"], check=False)
        meta.get("was_enabled")
        if meta and meta.get("was_enabled") is False:
            run(["pfctl", "-d"], check=False)
        for path in (self.snapshot_path, self.meta_path):
            try:
                path.unlink()
            except OSError:
                pass
        LOG.info("pf killswitch removed")

    def _enabled(self) -> bool:
        _rc, out, _err = run(["pfctl", "-s", "info"], check=False)
        return "Status: Enabled" in out

    def status(self) -> Dict[str, Any]:
        _rc, info, _err = run(["pfctl", "-s", "info"], check=False)
        _rc2, rules, _err2 = run(["pfctl", "-s", "rules"], check=False)
        enabled = "Status: Enabled" in info
        installed = self.rules_path.exists() and "sepia_hops" in (rules or "")
        return {
            "backend": self.name,
            "enabled": enabled,
            "rules_installed": installed,
            "interface": self.detect_interface(),
            "rules_file": str(self.rules_path),
        }

    def visible_rules(self) -> str:
        return run(["pfctl", "-s", "rules"], check=False)[1]


class MacSystemProxy(SystemProxy):
    name = "networksetup"

    def available(self) -> bool:
        return have("networksetup")

    # -- service discovery ------------------------------------------------- #

    def active_service(self) -> Optional[str]:
        iface = PfFirewall(Path(".")).detect_interface()
        services = self._services()
        if iface:
            _rc, order, _ = run(["networksetup", "-listnetworkserviceorder"], check=False)
            current = None
            for line in order.splitlines():
                header = re.match(r"\(\d+\)\s+(.*)", line.strip())
                if header:
                    current = header.group(1)
                    continue
                dev = re.search(r"Device:\s*(\S+?)\)", line)
                if dev and current and dev.group(1) == iface:
                    return current
        # Fall back to the first service that looks like a primary NIC.
        for candidate in ("Wi-Fi", "Ethernet", "Thunderbolt Ethernet", "USB 10/100/1000 LAN"):
            if candidate in services:
                return candidate
        return services[0] if services else None

    def _services(self) -> List[str]:
        _rc, out, _ = run(["networksetup", "-listallnetworkservices"], check=False)
        services = []
        for line in out.splitlines():
            line = line.strip()
            if not line or line.startswith("An asterisk"):
                continue
            services.append(line.lstrip("* ").strip())
        return services

    # -- snapshot / apply -------------------------------------------------- #

    def snapshot(self) -> Dict[str, Any]:
        service = self.active_service()
        if not service:
            raise PlatformError("could not determine the active network service")
        data: Dict[str, Any] = {"service": service}
        for key, flag in (
            ("web", "-getwebproxy"),
            ("secure_web", "-getsecurewebproxy"),
            ("socks", "-getsocksfirewallproxy"),
        ):
            _rc, out, _ = run(["networksetup", flag, service], check=False)
            enabled = re.search(r"Enabled:\s*(\w+)", out)
            server = re.search(r"Server:\s*(\S*)", out)
            port = re.search(r"Port:\s*(\d*)", out)
            data[key] = {
                "enabled": (enabled.group(1) if enabled else "No"),
                "server": server.group(1) if server else "",
                "port": port.group(1) if port else "",
            }
        _rc, out, _ = run(["networksetup", "-getdnsservers", service], check=False)
        data["dns"] = [line.strip() for line in out.splitlines()
                       if line.strip() and not line.lower().startswith("there aren")]
        _rc, out, _ = run(["networksetup", "-getproxybypassdomains", service], check=False)
        data["bypass"] = [line.strip() for line in out.splitlines()
                          if line.strip() and not line.lower().startswith("there aren")]
        return data

    def apply(self, http_host: str, http_port: int, socks_host: str, socks_port: int,
              dns_host: Optional[str] = None, dns_port: Optional[int] = None,
              bypass: Optional[List[str]] = None) -> None:
        service = self.snapshot().get("service")
        if not service:
            raise PlatformError("no active network service")
        service = str(service)
        run(["networksetup", "-setwebproxy", service, http_host, str(http_port)])
        run(["networksetup", "-setsecurewebproxy", service, http_host, str(http_port)])
        run(["networksetup", "-setwebproxystate", service, "on"])
        run(["networksetup", "-setsecurewebproxystate", service, "on"])
        run(["networksetup", "-setsocksfirewallproxy", service, socks_host, str(socks_port)])
        run(["networksetup", "-setsocksfirewallproxystate", service, "on"])
        domains = bypass or ["127.0.0.1", "localhost", "::1"]
        run(["networksetup", "-setproxybypassdomains", service] + domains)
        if dns_host:
            run(["networksetup", "-setdnsservers", service, dns_host])
        run(["dscacheutil", "-flushcache"], check=False)
        run(["killall", "-HUP", "mDNSResponder"], check=False)
        LOG.info("system proxy applied on service %r", service)

    def restore(self, snapshot: Dict[str, Any]) -> None:
        service = str(snapshot.get("service") or "")
        if not service:
            return
        for key, flag_on, flag_off, setter in (
            ("web", "on", "off", "-setwebproxy"),
            ("secure_web", "on", "off", "-setsecurewebproxy"),
            ("socks", "on", "off", "-setsocksfirewallproxy"),
        ):
            entry = snapshot.get(key) or {}
            state_flag = "-setwebproxystate" if key == "web" else (
                "-setsecurewebproxystate" if key == "secure_web" else "-setsocksfirewallproxystate")
            if str(entry.get("enabled", "No")).lower() == "yes" and entry.get("server"):
                run(["networksetup", setter, service, str(entry["server"]), str(entry["port"])])
                run(["networksetup", state_flag, service, flag_on])
            else:
                run(["networksetup", state_flag, service, flag_off], check=False)
        dns = snapshot.get("dns") or []
        if dns:
            run(["networksetup", "-setdnsservers", service] + [str(d) for d in dns], check=False)
        else:
            run(["networksetup", "-setdnsservers", service, "Empty"], check=False)
        bypass = snapshot.get("bypass") or []
        if bypass:
            run(["networksetup", "-setproxybypassdomains", service] + [str(b) for b in bypass],
                check=False)
        else:
            run(["networksetup", "-setproxybypassdomains", service, "Empty"], check=False)
        run(["dscacheutil", "-flushcache"], check=False)
        run(["killall", "-HUP", "mDNSResponder"], check=False)
        LOG.info("system proxy restored on service %r", service)
