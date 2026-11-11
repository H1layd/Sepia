"""Firewall / system-proxy backends shared interface."""
from __future__ import annotations

import ipaddress
import os
import shutil
import socket
import subprocess
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

from .. import util

LOG = util.get_logger("platform")


class PlatformError(RuntimeError):
    pass


class CommandError(PlatformError):
    def __init__(self, cmd: Sequence[str], rc: int, out: str, err: str):
        super().__init__(f"{' '.join(cmd)} failed (rc={rc}): {(err or out).strip()[:400]}")
        self.cmd = list(cmd)
        self.rc = rc
        self.out = out
        self.err = err


def run(cmd: Sequence[str], check: bool = True, stdin: Optional[str] = None,
        timeout: int = 60) -> Tuple[int, str, str]:
    LOG.debug("exec: %s", " ".join(cmd))
    try:
        proc = subprocess.run(
            list(cmd),
            input=stdin.encode() if stdin is not None else None,
            capture_output=True,
            timeout=timeout,
        )
    except FileNotFoundError as exc:
        raise PlatformError(f"missing binary: {cmd[0]}") from exc
    out = proc.stdout.decode("utf-8", "replace")
    err = proc.stderr.decode("utf-8", "replace")
    if check and proc.returncode != 0:
        raise CommandError(cmd, proc.returncode, out, err)
    return proc.returncode, out, err


def have(binary: str) -> bool:
    return shutil.which(binary) is not None


def require_root() -> None:
    if os.geteuid() != 0:
        raise PlatformError("this operation needs root: re-run with sudo")


class FirewallBackend:
    """Renders and installs a fail-closed ruleset around the local proxy."""

    name = "generic"
    supports_transparent = False
    supports_original_dst = False
    transparent_scope = "none"  # none | http | all-tcp

    def available(self) -> bool:
        raise NotImplementedError

    def detect_interface(self) -> Optional[str]:
        """Interface with the default route, for rules that need a device name."""
        return None

    def build_ruleset(self, cfg: Dict[str, Any], allowlist: List[Tuple[str, int]],
                      iface: Optional[str]) -> str:
        raise NotImplementedError

    def apply(self, ruleset: str) -> None:
        raise NotImplementedError

    def teardown(self) -> None:
        raise NotImplementedError

    def status(self) -> Dict[str, Any]:
        raise NotImplementedError

    def visible_rules(self) -> str:
        return ""


class SystemProxy:
    """Push the local proxy into OS-level settings so GUI apps use it."""

    name = "generic"

    def available(self) -> bool:
        raise NotImplementedError

    def snapshot(self) -> Dict[str, Any]:
        raise NotImplementedError

    def apply(self, http_host: str, http_port: int, socks_host: str, socks_port: int,
              dns_host: Optional[str] = None, dns_port: Optional[int] = None,
              bypass: Optional[List[str]] = None) -> None:
        raise NotImplementedError

    def restore(self, snapshot: Dict[str, Any]) -> None:
        raise NotImplementedError


# --------------------------------------------------------------------------- #
# shared helpers
# --------------------------------------------------------------------------- #


def ipv4_complement(keep: List[str], extra_keep: List[str]) -> List[str]:
    """All IPv4 space minus *keep*, expressed as the fewest CIDR blocks.

    Windows Firewall cannot say 'block everything except X', so on Windows the
    killswitch is built from the complement: block rules over these ranges, and
    the hop addresses stay outside every block rule (default-allow outbound).
    """
    keepnets = []
    for addr in list(keep) + list(extra_keep):
        try:
            net = ipaddress.ip_network(addr, strict=False)
        except ValueError:
            continue
        if net.version == 4:
            keepnets.append(net)
    universe = ipaddress.ip_network("0.0.0.0/0")
    remaining = [universe]
    for net in keepnets:
        nxt: List[ipaddress.IPv4Network] = []
        for candidate in remaining:
            if not candidate.overlaps(net):
                nxt.append(candidate)
                continue
            if net.subnet_of(candidate):
                nxt.extend(candidate.address_exclude(net))
            elif candidate.subnet_of(net):
                continue
            else:
                for piece in candidate.address_exclude(
                        ipaddress.ip_network(f"{net.network_address}/{net.prefixlen}", strict=False)
                        if False else net):
                    nxt.append(piece)
        remaining = []
        merged: List[ipaddress.IPv4Network] = []
        for candidate in nxt:
            if any(candidate.subnet_of(m) for m in merged):
                continue
            merged = [m for m in merged if not m.subnet_of(candidate)]
            merged.append(candidate)
        remaining = merged
    return [str(net) for net in ipaddress.collapse_addresses(remaining) if net.prefixlen > 0]


def uniq(values: List[str]) -> List[str]:
    seen = set()
    out = []
    for value in values:
        if value not in seen:
            seen.add(value)
            out.append(value)
    return out


def hostname_iface_hint() -> Optional[str]:
    """Best-effort default interface via a UDP connect trick (no packets sent)."""
    for family, probe in ((socket.AF_INET, "1.1.1.1"), (socket.AF_INET6, "2606:4700:4700::1111")):
        try:
            sock = socket.socket(family, socket.SOCK_DGRAM)
            sock.connect((probe, 80))
            local = sock.getsockname()[0]
            sock.close()
            for name, addresses in local_addresses().items():
                if local in addresses:
                    return name
        except OSError:
            continue
    return None


def local_addresses() -> Dict[str, List[str]]:
    """Parse the OS interface table without extra dependencies."""
    result: Dict[str, List[str]] = {}
    if util.platform() == "macos":
        rc, out, _ = run(["ifconfig"], check=False)
    elif util.platform() == "linux":
        rc, out, _ = run(["ip", "-o", "addr", "show"], check=False)
    else:
        return result
    current = None
    for line in out.splitlines():
        if util.platform() == "macos":
            if line and not line[0].isspace() and ":" in line:
                current = line.split(":", 1)[0]
                result.setdefault(current, [])
            elif "inet " in line and current:
                token = line.split("inet ", 1)[1].split()[0]
                result[current].append(token.split("/")[0])
        else:
            parts = line.split()
            if len(parts) > 3 and parts[1] == "inet":
                result.setdefault(parts[1] if False else parts[0], [])
                result[parts[0]].append(parts[3].split("/")[0])
    return {k: v for k, v in result.items() if v}
