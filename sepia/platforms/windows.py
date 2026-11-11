"""Windows backend: firewall killswitch (complement block rules) + WinINET proxy."""
from __future__ import annotations

import json
import re
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from .. import util
from .base import (FirewallBackend, PlatformError, SystemProxy, have,
                   ipv4_complement, require_root, run)

LOG = util.get_logger("windows")

PS = ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]
RULE_PREFIX = "sepia-"


class WindowsFirewall(FirewallBackend):
    name = "Windows Firewall"
    supports_transparent = False
    supports_original_dst = False
    transparent_scope = "none"

    def __init__(self, run_dir: Path):
        self.run_dir = Path(run_dir)
        self.script_path = self.run_dir / "windows-firewall.ps1"

    def available(self) -> bool:
        return have("powershell.exe") or have("pwsh.exe")

    def detect_interface(self) -> Optional[str]:
        ps = _ps()
        rc, out, _ = run(ps + [
            "-Command",
            "(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | "
            "Sort-Object RouteMetric | Select-Object -First 1).InterfaceAlias",
        ], check=False)
        name = out.strip().splitlines()[-1].strip() if out.strip() else ""
        return name or None

    # -- rules ------------------------------------------------------------- #

    def build_ruleset(self, cfg: Dict[str, Any], allowlist: List[Tuple[str, int]],
                      iface: Optional[str]) -> str:
        fw = cfg["firewall"]
        keep = ["127.0.0.0/8", "169.254.0.0/16"]  # loopback + link-local
        if fw.get("allow_lan"):
            keep += ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]
        ranges = ipv4_complement([ip for ip, _ in allowlist], keep)

        lines = [
            "# sepia Windows killswitch, generated " + time.strftime("%Y-%m-%dT%H:%M:%S"),
            "$ErrorActionPreference = 'Stop'",
            "Get-NetFirewallRule -DisplayName 'sepia-*' -ErrorAction SilentlyContinue | "
            "Remove-NetFirewallRule -ErrorAction SilentlyContinue",
        ]
        chunk = 40
        for index in range(0, len(ranges), chunk):
            block = ranges[index:index + chunk]
            quoted = ",".join(f"'{r}'" for r in block)
            lines.append(
                f"New-NetFirewallRule -DisplayName 'sepia-block-v4-{index // chunk + 1}' "
                f"-Direction Outbound -Action Block -Profile Any -Enabled True "
                f"-RemoteAddress @({quoted}) | Out-Null"
            )
        if fw.get("block_ipv6", True):
            lines.append(
                "New-NetFirewallRule -DisplayName 'sepia-block-v6' -Direction Outbound "
                "-Action Block -Profile Any -Enabled True -RemoteAddress '::/0' | Out-Null"
            )
        allowed = sorted({f"{ip}:{port}" for ip, port in allowlist})
        lines.append("# allowed hop endpoints: " + (", ".join(allowed) or "(none)"))
        lines.append("Write-Output 'sepia-rules-installed'")
        return "\n".join(lines) + "\n"

    # -- apply / teardown -------------------------------------------------- #

    def apply(self, ruleset: str) -> None:
        require_root()
        self.run_dir.mkdir(parents=True, exist_ok=True)
        util.atomic_write(self.script_path, ruleset)
        ps = _ps()
        run(ps + ["-File", str(self.script_path)], timeout=180)
        LOG.info("Windows firewall rules installed")

    def teardown(self) -> None:
        require_root()
        script = (
            "Get-NetFirewallRule -DisplayName 'sepia-*' -ErrorAction SilentlyContinue | "
            "Remove-NetFirewallRule -ErrorAction SilentlyContinue; Write-Output 'sepia-rules-removed'"
        )
        run(_ps() + ["-Command", script], check=False, timeout=120)
        LOG.info("Windows firewall rules removed")

    def status(self) -> Dict[str, Any]:
        ps = _ps()
        rc, out, _ = run(ps + [
            "-Command",
            "@(Get-NetFirewallRule -DisplayName 'sepia-*' -ErrorAction SilentlyContinue).Count",
        ], check=False)
        count = 0
        if out.strip():
            try:
                count = int(out.strip().splitlines()[-1])
            except ValueError:
                count = 0
        return {
            "backend": self.name,
            "enabled": count > 0,
            "rules_installed": count > 0,
            "rule_count": count,
            "interface": self.detect_interface(),
        }

    def visible_rules(self) -> str:
        ps = _ps()
        _rc, out, _ = run(ps + [
            "-Command",
            "Get-NetFirewallRule -DisplayName 'sepia-*' | Format-Table DisplayName,Enabled,"
            "Direction,Action -AutoSize | Out-String -Width 200",
        ], check=False)
        return out


class WindowsSystemProxy(SystemProxy):
    name = "WinINET + WinHTTP"

    def __init__(self, run_dir: Path):
        self.run_dir = Path(run_dir)
        self.backup_path = self.run_dir / "windows-system-proxy.json"

    def available(self) -> bool:
        return have("reg.exe") and have("netsh.exe")

    def _iface(self) -> Optional[str]:
        return WindowsFirewall(self.run_dir).detect_interface()

    def _reg_get(self, key: str, name: str) -> Optional[str]:
        rc, out, _ = run(["reg.exe", "query", key, "/v", name], check=False)
        if rc != 0:
            return None
        match = re.search(rf"{re.escape(name)}\s+REG_\w+\s+(.*)", out)
        return match.group(1).strip() if match else None

    def snapshot(self) -> Dict[str, Any]:
        key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
        data: Dict[str, Any] = {
            "proxy_enable": self._reg_get(key, "ProxyEnable"),
            "proxy_server": self._reg_get(key, "ProxyServer"),
            "proxy_override": self._reg_get(key, "ProxyOverride"),
            "iface": self._iface(),
            "iface_dns": {},
        }
        _rc, out, _ = run(["netsh.exe", "winhttp", "show", "proxy"], check=False)
        data["winhttp"] = out.strip()
        iface = data.get("iface")
        if iface:
            for fam in ("ipv4", "ipv6"):
                rc, dns_out, _ = run(
                    ["netsh.exe", "interface", fam, "show", "dnsservers", f"name={iface}"],
                    check=False,
                )
                if rc == 0:
                    data["iface_dns"][fam] = dns_out.strip()
        util.write_json(self.backup_path, data)
        util.secure_file(self.backup_path)
        return data

    def apply(self, http_host: str, http_port: int, socks_host: str, socks_port: int,
              dns_host: Optional[str] = None, dns_port: Optional[int] = None,
              bypass: Optional[List[str]] = None) -> None:
        key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
        override = bypass or ["localhost", "127.*", "::1"]
        run(["reg.exe", "add", key, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "1", "/f"])
        run(["reg.exe", "add", key, "/v", "ProxyServer", "/t", "REG_SZ",
             "/d", f"{http_host}:{http_port}", "/f"])
        run(["reg.exe", "add", key, "/v", "ProxyOverride", "/t", "REG_SZ",
             "/d", ";".join(override), "/f"])
        run(["netsh.exe", "winhttp", "set", "proxy", f"{http_host}:{http_port}"], check=False)
        # Notify running applications that WinINET settings changed.
        ps = _ps()
        run(ps + [
            "-Command",
            "Add-Type -Namespace Win32 -Name Native -MemberDefinition "
            "'[DllImport(\"wininet.dll\")] public static extern bool InternetSetOption("
            "System.IntPtr h, int o, System.IntPtr b, int l);'; "
            "[Win32.Native]::InternetSetOption([System.IntPtr]::Zero, 39, "
            "[System.IntPtr]::Zero, 0) | Out-Null; "
            "[Win32.Native]::InternetSetOption([System.IntPtr]::Zero, 37, "
            "[System.IntPtr]::Zero, 0) | Out-Null",
        ], check=False)
        iface = self._iface()
        if dns_host and iface:
            run(["netsh.exe", "interface", "ipv4", "set", "dnsservers",
                 f"name={iface}", "static", dns_host, "primary"], check=False)
            run(["netsh.exe", "interface", "ipv6", "set", "dnsservers",
                 f"name={iface}", "static", "::1", "primary"], check=False)
            run(["ipconfig.exe", "/flushdns"], check=False)
        LOG.info("WinINET/WinHTTP proxy applied")

    def restore(self, snapshot: Dict[str, Any]) -> None:
        key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
        enable = str(snapshot.get("proxy_enable") or "0")
        run(["reg.exe", "add", key, "/v", "ProxyEnable", "/t", "REG_DWORD",
             "/d", "1" if enable not in ("0", "0x0") else "0", "/f"], check=False)
        if snapshot.get("proxy_server"):
            run(["reg.exe", "add", key, "/v", "ProxyServer", "/t", "REG_SZ",
                 "/d", str(snapshot["proxy_server"]), "/f"], check=False)
        if snapshot.get("proxy_override") is not None:
            run(["reg.exe", "add", key, "/v", "ProxyOverride", "/t", "REG_SZ",
                 "/d", str(snapshot["proxy_override"]), "/f"], check=False)
        run(["netsh.exe", "winhttp", "reset", "proxy"], check=False)
        iface = snapshot.get("iface")
        if iface:
            run(["netsh.exe", "interface", "ipv4", "set", "dnsservers",
                 f"name={iface}", "dhcp"], check=False)
            run(["netsh.exe", "interface", "ipv6", "set", "dnsservers",
                 f"name={iface}", "dhcp"], check=False)
            run(["ipconfig.exe", "/flushdns"], check=False)
        LOG.info("WinINET/WinHTTP proxy restored")


def _ps() -> List[str]:
    if have("pwsh.exe"):
        return ["pwsh.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass"]
    return list(PS)
