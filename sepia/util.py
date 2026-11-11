"""Shared helpers: logging, host/port maths, state files, small syscalls."""
from __future__ import annotations

import ipaddress
import json
import logging
import os
import secrets
import socket
import stat
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

APP_NAME = "sepia"


def env(name: str, default=None):
    """SEPIA_* wins; the old VEIL_* names still work for continuity."""
    for prefix in ("SEPIA_", "VEIL_"):
        value = os.environ.get(prefix + name)
        if value is not None:
            return value
    return default

LOG_FORMAT = "%(asctime)s %(levelname)-7s %(name)-12s %(message)s"
LOG_DATE = "%H:%M:%S"


def setup_logging(verbose: bool = False, logfile: Optional[Path] = None) -> logging.Logger:
    root = logging.getLogger(APP_NAME)
    if root.handlers:
        return root
    root.setLevel(logging.DEBUG if verbose else logging.INFO)
    root.propagate = False
    fmt = logging.Formatter(LOG_FORMAT, LOG_DATE)

    stream = logging.StreamHandler(sys.stderr)
    stream.setFormatter(fmt)
    stream.setLevel(logging.DEBUG if verbose else logging.INFO)
    root.addHandler(stream)

    if logfile is not None:
        logfile.parent.mkdir(parents=True, exist_ok=True)
        fh = logging.FileHandler(logfile)
        fh.setFormatter(logging.Formatter("%(asctime)s %(levelname)-7s %(name)-12s %(message)s"))
        fh.setLevel(logging.DEBUG)
        root.addHandler(fh)
    return root


def get_logger(name: str) -> logging.Logger:
    return logging.getLogger(f"{APP_NAME}.{name}")


def redact_hostport(value: str) -> str:
    """Never let credentials reach a log file / terminal scrollback."""
    if "@" in value:
        creds, _, host = value.rpartition("@")
        if ":" in creds:
            user, _, _pw = creds.partition(":")
            return f"{user}:***@{host}"
        return f"***@{host}"
    return value


def split_hostport(value: str, default_port: int = 0) -> Tuple[str, int]:
    """Accepts '1.2.3.4:1080', '[::1]:1080', 'host' and 'host:port'."""
    value = value.strip()
    if value.startswith("["):
        host, _, rest = value[1:].partition("]")
        port = int(rest.lstrip(":") or default_port)
        return host, port
    if value.count(":") == 1:
        host, _, port_s = value.partition(":")
        return host, int(port_s)
    return value, default_port


def join_hostport(host: str, port: int) -> str:
    return f"[{host}]:{port}" if ":" in host else f"{host}:{port}"


def is_ip(value: str) -> bool:
    try:
        ipaddress.ip_address(value)
        return True
    except ValueError:
        return False


def ip_version(value: str) -> int:
    return ipaddress.ip_address(value).version


def rand_hex(nbytes: int = 8) -> str:
    return secrets.token_hex(nbytes)


def int_id() -> int:
    """Random 16-bit id for hand-built DNS queries."""
    return secrets.randbelow(65536)


def now() -> float:
    return time.time()


def human_bytes(n: int) -> str:
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if n < 1024 or unit == "TiB":
            return f"{n:.0f}{unit}" if unit == "B" else f"{n:.1f}{unit}"
        n /= 1024.0
    return f"{n:.1f}TiB"


def human_duration(seconds: float) -> str:
    seconds = max(0, int(seconds))
    days, rem = divmod(seconds, 86400)
    hours, rem = divmod(rem, 3600)
    mins, secs = divmod(rem, 60)
    if days:
        return f"{days}d{hours:02d}h"
    if hours:
        return f"{hours}h{mins:02d}m"
    if mins:
        return f"{mins}m{secs:02d}s"
    return f"{secs}s"


def atomic_write(path: Path, text: str, mode: Optional[int] = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=f".{path.name}.")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write(text)
            fh.flush()
            os.fsync(fh.fileno())
        if mode is not None:
            os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def write_json(path: Path, payload: Dict[str, Any], mode: Optional[int] = None) -> None:
    atomic_write(path, json.dumps(payload, indent=2, sort_keys=False) + "\n", mode)


def read_json(path: Path, default: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
        if not isinstance(data, dict):
            raise ValueError("top-level JSON value is not an object")
        return data
    except FileNotFoundError:
        return dict(default or {})
    except (ValueError, OSError):
        return dict(default or {})


def secure_file(path: Path) -> None:
    """chmod 600 for anything holding credentials."""
    try:
        os.chmod(path, stat.S_IRUSR | stat.S_IWUSR)
    except OSError:
        pass


def is_root() -> bool:
    return os.geteuid() == 0


def resolve_v4(host: str, timeout: float = 3.0) -> list:
    """Resolve before the killswitch goes up. Returns unique IPv4 strings."""
    out = []
    try:
        infos = socket.getaddrinfo(host, None, socket.AF_INET, socket.SOCK_STREAM)
    except OSError:
        return out
    for info in infos:
        addr = info[4][0]
        if addr not in out:
            out.append(addr)
    return out


def resolve_any(host: str) -> list:
    out = []
    try:
        infos = socket.getaddrinfo(host, None, 0, socket.SOCK_STREAM)
    except OSError:
        return out
    for info in infos:
        addr = info[4][0]
        if addr not in out:
            out.append(addr)
    return out


EGRESS_MARK = 0x1
SO_MARK = 36  # Linux: wipe the mark on sockets that leave through the chain


def mark_socket(sock: "socket.socket") -> bool:
    """Mark a socket so nftables never redirects it back into our own listener.

    Without this, a local first hop (or the chain dialling 80/443 directly) is
    caught by our own REDIRECT and loops until the process runs out of fds.
    """
    if platform() != "linux":
        return False
    try:
        sock.setsockopt(socket.SOL_SOCKET, SO_MARK, EGRESS_MARK)
        return True
    except OSError:
        return False


def decode_hostname(raw: bytes) -> str:
    """Wire names are ASCII (punycode). Preserve that form so rule matching is
    stable, but tolerate UTF-8 from sloppy clients."""
    try:
        return raw.decode("ascii")
    except UnicodeDecodeError:
        pass
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("latin-1")


def clamp(value: int, low: int, high: int) -> int:
    return max(low, min(high, value))


def platform() -> str:
    if sys.platform == "darwin":
        return "macos"
    if sys.platform.startswith("linux"):
        return "linux"
    return sys.platform
