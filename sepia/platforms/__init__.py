"""Platform backend selection."""
from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, Optional

from .. import util
from .base import FirewallBackend, PlatformError, SystemProxy

__all__ = ["FirewallBackend", "SystemProxy", "PlatformError",
           "get_firewall", "get_system_proxy", "capabilities"]


def get_firewall(run_dir: Path) -> FirewallBackend:
    if util.platform() == "macos":
        from .darwin import PfFirewall

        return PfFirewall(run_dir)
    if util.platform() == "linux":
        from .linux import NftFirewall

        return NftFirewall(run_dir)
    if util.platform() == "win32" or util.platform().startswith("win"):
        from .windows import WindowsFirewall

        return WindowsFirewall(run_dir)
    raise PlatformError(f"no firewall backend for platform {util.platform()!r}")


def get_system_proxy(run_dir: Path) -> SystemProxy:
    if util.platform() == "macos":
        from .darwin import MacSystemProxy

        return MacSystemProxy()
    if util.platform() == "linux":
        from .linux import LinuxSystemProxy

        return LinuxSystemProxy(run_dir)
    if util.platform() == "win32" or util.platform().startswith("win"):
        from .windows import WindowsSystemProxy

        return WindowsSystemProxy(run_dir)
    raise PlatformError(f"no system-proxy backend for platform {util.platform()!r}")


def capabilities(run_dir: Path) -> Dict[str, Any]:
    out: Dict[str, Any] = {"platform": util.platform()}
    try:
        fw = get_firewall(run_dir)
        out["firewall"] = {
            "name": fw.name,
            "available": fw.available(),
            "transparent": fw.supports_transparent,
            "transparent_scope": fw.transparent_scope,
            "original_destination": fw.supports_original_dst,
            "interface": fw.detect_interface(),
        }
    except PlatformError as exc:
        out["firewall"] = {"error": str(exc)}
    try:
        sp = get_system_proxy(run_dir)
        out["system_proxy"] = {"name": sp.name, "available": sp.available()}
    except PlatformError as exc:
        out["system_proxy"] = {"error": str(exc)}
    return out
