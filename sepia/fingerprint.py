"""Egress fingerprinting: what our TLS handshake looks like, what the host leaks."""
from __future__ import annotations

import hashlib
import socket
import ssl
import struct
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from . import util

LOG = util.get_logger("fingerprint")

GREASE = {0x0A0A, 0x1A1A, 0x2A2A, 0x3A3A, 0x4A4A, 0x5A5A, 0x6A6A, 0x7A7A,
          0x8A8A, 0x9A9A, 0xAAAA, 0xBABA, 0xCACA, 0xDADA, 0xEAEA, 0xFAFA}

EXT_SERVER_NAME = 0
EXT_SUPPORTED_GROUPS = 10
EXT_EC_POINT_FORMATS = 11
EXT_ALPN = 16
EXT_SUPPORTED_VERSIONS = 43


def capture_client_hello(host: str = "cloudflare.com", alpn: Optional[List[str]] = None
                         ) -> bytes:
    """Produce the exact bytes of a ClientHello without touching the network."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    if alpn is not None:
        try:
            context.set_alpn_protocols(alpn)
        except NotImplementedError:
            pass
    incoming = ssl.MemoryBIO()
    outgoing = ssl.MemoryBIO()
    obj = context.wrap_bio(incoming, outgoing, server_hostname=host)
    try:
        obj.do_handshake()
    except ssl.SSLWantReadError:
        pass
    data = outgoing.read()
    if not data.startswith(b"\x16"):
        raise RuntimeError("did not capture a TLS handshake record")
    return data


def parse_client_hello(data: bytes) -> Dict[str, Any]:
    if len(data) < 5 + 4:
        raise ValueError("handshake too short")
    record_type = data[0]
    record_version = struct.unpack("!H", data[1:3])[0]
    record_len = struct.unpack("!H", data[3:5])[0]
    if record_type != 0x16:
        raise ValueError(f"not a handshake record (type {record_type:#x})")
    handshake = data[5:5 + record_len]
    hs_type = handshake[0]
    if hs_type != 0x01:
        raise ValueError(f"not a ClientHello (type {hs_type:#x})")
    body = handshake[4:]
    offset = 0
    client_version = struct.unpack("!H", body[offset:offset + 2])[0]
    offset += 2 + 32  # version + random
    sid_len = body[offset]
    offset += 1
    session_id = body[offset:offset + sid_len]
    offset += sid_len
    ciphers_len = struct.unpack("!H", body[offset:offset + 2])[0]
    offset += 2
    cipher_bytes = body[offset:offset + ciphers_len]
    offset += ciphers_len
    comp_len = body[offset]
    offset += 1 + comp_len

    ciphers = [struct.unpack("!H", cipher_bytes[i:i + 2])[0]
               for i in range(0, len(cipher_bytes) - 1, 2)]
    ciphers = [c for c in ciphers if c not in GREASE]

    extensions: List[int] = []
    groups: List[int] = []
    point_formats: List[int] = []
    alpn_protocols: List[str] = []
    sni: Optional[str] = None
    supported_versions: List[int] = []

    if offset + 2 <= len(body):
        ext_total = struct.unpack("!H", body[offset:offset + 2])[0]
        offset += 2
        end = offset + ext_total
        while offset + 4 <= end:
            ext_type, ext_len = struct.unpack("!HH", body[offset:offset + 4])
            offset += 4
            ext_data = body[offset:offset + ext_len]
            offset += ext_len
            if ext_type in GREASE:
                continue
            extensions.append(ext_type)
            if ext_type == EXT_SERVER_NAME and len(ext_data) > 5:
                name_len = struct.unpack("!H", ext_data[3:5])[0]
                sni = util.decode_hostname(ext_data[5:5 + name_len])
            elif ext_type == EXT_SUPPORTED_GROUPS and len(ext_data) >= 2:
                count = struct.unpack("!H", ext_data[:2])[0]
                for i in range(count):
                    start = 2 + i * 2
                    if start + 2 > len(ext_data):
                        break
                    value = struct.unpack("!H", ext_data[start:start + 2])[0]
                    if value not in GREASE:
                        groups.append(value)
            elif ext_type == EXT_EC_POINT_FORMATS and len(ext_data) >= 1:
                point_formats = list(ext_data[1:1 + ext_data[0]])
            elif ext_type == EXT_ALPN and len(ext_data) >= 2:
                alpn_len = struct.unpack("!H", ext_data[:2])[0]
                cursor = 2
                while cursor < 2 + alpn_len:
                    name_len = ext_data[cursor]
                    alpn_protocols.append(
                        ext_data[cursor + 1:cursor + 1 + name_len].decode("ascii", "replace"))
                    cursor += 1 + name_len
            elif ext_type == EXT_SUPPORTED_VERSIONS and len(ext_data) >= 1:
                count = ext_data[0]
                for i in range(count):
                    start = 1 + i * 2
                    if start + 2 > len(ext_data):
                        break
                    supported_versions.append(struct.unpack("!H", ext_data[start:start + 2])[0])

    ja3_string = ",".join([
        str(client_version),
        "-".join(str(c) for c in ciphers),
        "-".join(str(e) for e in extensions),
        "-".join(str(g) for g in groups),
        "-".join(str(p) for p in point_formats),
    ])
    return {
        "ja3": ja3_string,
        "ja3_md5": hashlib.md5(ja3_string.encode()).hexdigest(),
        "record_version": f"0x{record_version:04x}",
        "client_version": f"0x{client_version:04x}",
        "sni": sni,
        "alpn": alpn_protocols,
        "cipher_count": len(ciphers),
        "extension_count": len(extensions),
        "extensions": extensions,
        "groups": groups,
        "point_formats": point_formats,
        "supported_versions": [f"0x{v:04x}" for v in supported_versions],
        "session_ticket": 35 in extensions,
        "size_bytes": len(data),
    }


def egress_tls_report(host: str = "cloudflare.com") -> Dict[str, Any]:
    report: Dict[str, Any] = {}
    for label, alpn in (("default", None), ("http/1.1", ["http/1.1"]),
                        ("h2_and_http1", ["h2", "http/1.1"])):
        try:
            raw = capture_client_hello(host, alpn)
            report[label] = parse_client_hello(raw)
        except Exception as exc:  # noqa: BLE001
            report[label] = {"error": f"{type(exc).__name__}: {exc}"}
    return report


# --------------------------------------------------------------------------- #
# host-side leak surface
# --------------------------------------------------------------------------- #


def hostname_leaks() -> List[Dict[str, str]]:
    """Names the machine hands out to the local network for free."""
    problems: List[Dict[str, str]] = []
    hostname = socket.gethostname()
    if hostname:
        problems.append({
            "surface": "hostname",
            "value": hostname,
            "note": "broadcast to the LAN via DHCP and mDNS",
        })
    fqdn = socket.getfqdn()
    if fqdn and fqdn != hostname:
        problems.append({"surface": "fqdn", "value": fqdn,
                         "note": "resolvable name tied to this machine"})
    return problems


def ipv6_exposure(interface_table: Optional[Dict[str, List[str]]] = None) -> Dict[str, Any]:
    from .platforms import base as pbase

    table = interface_table or pbase.local_addresses()
    global_v6: List[str] = []
    link_local_v6: List[str] = []
    for name, addresses in table.items():
        for addr in addresses:
            if ":" not in addr:
                continue
            if addr.lower().startswith("fe80"):
                link_local_v6.append(f"{name}:{addr}")
            elif addr.lower() not in ("::1",) and not addr.lower().startswith("fd"):
                global_v6.append(f"{name}:{addr}")
    return {
        "interfaces": table,
        "global_ipv6": global_v6,
        "link_local_ipv6": link_local_v6,
        "leaky": bool(global_v6),
    }


def configured_dns() -> Dict[str, Any]:
    platform = util.platform()
    from .platforms.base import run

    servers: List[str] = []
    raw = ""
    if platform == "macos":
        _rc, raw, _ = run(["scutil", "--dns"], check=False)
        for line in raw.splitlines():
            stripped = line.strip()
            if stripped.startswith("nameserver["):
                servers.append(stripped.split(":", 1)[1].strip())
    elif platform == "linux":
        path = Path("/etc/resolv.conf")
        if path.exists():
            raw = path.read_text(encoding="utf-8", errors="replace")
            for line in raw.splitlines():
                if line.strip().startswith("nameserver"):
                    servers.append(line.split()[1])
    elif platform.startswith("win"):
        _rc, raw, _ = run(["ipconfig.exe", "/all"], check=False)
        servers = [line.split(":", 1)[1].strip() for line in raw.splitlines()
                   if "DNS Servers" in line and ":" in line]
    unique = sorted(set(servers))
    loopback_only = bool(unique) and all(s in ("127.0.0.1", "::1") for s in unique)
    return {"servers": unique, "loopback_only": loopback_only, "raw": raw[:2000]}


def tcp_stack_report() -> Dict[str, Any]:
    from .platforms.base import run

    if util.platform() != "macos":
        return {}
    wanted = {
        "net.inet.tcp.blackhole": "drop silent-scanned TCP segments",
        "net.inet.udp.blackhole": "drop silent-scanned UDP segments",
        "net.inet.icmp.icmplim": "rate-limit ICMP replies",
        "net.inet.tcp.mssdflt": "default MSS",
        "net.inet.tcp.keepidle": "TCP keepalive idle",
        "net.inet.ip.forwarding": "IP forwarding",
        "net.inet6.ip6.forwarding": "IPv6 forwarding",
    }
    out: Dict[str, Any] = {}
    for key, note in wanted.items():
        rc, value, _ = run(["sysctl", "-n", key], check=False)
        out[key] = {"value": value.strip() if rc == 0 else None, "note": note}
    return out


def audit() -> Dict[str, Any]:
    return {
        "platform": util.platform(),
        "hostname": socket.gethostname(),
        "hostname_surfaces": hostname_leaks(),
        "ipv6": ipv6_exposure(),
        "dns": configured_dns(),
        "tls_egress": egress_tls_report(),
        "tcp_stack": tcp_stack_report(),
        "locals": _local_listening_hints(),
    }


def _local_listening_hints() -> List[str]:
    from .platforms.base import run

    rc, out, _ = run(["lsof", "-nP", "-iTCP", "-sTCP:LISTEN"], check=False)
    if rc != 0:
        return []
    lines = [line for line in out.splitlines()[1:] if line.strip()]
    return lines[:40]
