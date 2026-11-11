"""Proxy chain: ordered hops, routing rules, health probes."""
from __future__ import annotations

import asyncio
import fnmatch
import socket
import time
from typing import Any, Dict, List, Optional, Tuple

from . import httpc, socks5, util

LOG = util.get_logger("chain")

ACTIONS = ("chain", "direct", "block")


async def open_marked_connection(host: str, port: int, timeout: float
                                 ) -> Tuple[asyncio.StreamReader, asyncio.StreamWriter]:
    """Open a TCP connection whose packets carry the sepia egress mark (Linux)."""
    if util.platform() != "linux":
        return await asyncio.wait_for(asyncio.open_connection(host, port), timeout=timeout)
    ready = asyncio.Event()

    async def _connect() -> Tuple[asyncio.StreamReader, asyncio.StreamWriter]:
        loop = asyncio.get_running_loop()
        infos = await loop.getaddrinfo(host, port, type=socket.SOCK_STREAM)
        last_error: Optional[BaseException] = None
        for family, socktype, proto, _canon, sockaddr in infos:
            sock = socket.socket(family, socktype, proto)
            sock.setblocking(False)
            util.mark_socket(sock)
            try:
                await loop.sock_connect(sock, sockaddr)
                reader, writer = await asyncio.open_connection(sock=sock)
                return reader, writer
            except OSError as exc:
                last_error = exc
                sock.close()
        raise last_error or OSError(f"cannot connect to {host}:{port}")

    return await asyncio.wait_for(_connect(), timeout=timeout)


class HopError(Exception):
    pass


class Hop:
    __slots__ = ("kind", "host", "port", "username", "password", "label")

    def __init__(self, kind: str, host: str, port: int,
                 username: Optional[str] = None, password: Optional[str] = None,
                 label: str = ""):
        self.kind = kind
        self.host = host
        self.port = port
        self.username = username
        self.password = password
        self.label = label or util.join_hostport(host, port)

    @classmethod
    def from_config(cls, raw: Dict[str, Any], index: int) -> "Hop":
        host, port = util.split_hostport(str(raw["addr"]))
        return cls(
            "http" if str(raw.get("type")) == "http" else "socks5",
            host,
            port,
            raw.get("username"),
            raw.get("password"),
            raw.get("label") or f"hop{index + 1}",
        )

    def describe(self) -> str:
        return f"{self.label} ({self.kind} {util.redact_hostport(util.join_hostport(self.host, self.port))})"

    def __repr__(self) -> str:  # pragma: no cover - debug only
        return f"<Hop {self.label} {self.kind} {self.host}:{self.port}>"


class Chain:
    """Dial targets through a list of hops.

    Hop i is reached *through* hops 0..i-1, so the final destination only ever
    sees the last hop's address. Hops are never contacted directly in parallel,
    which keeps the path strictly linear and auditable.
    """

    def __init__(self, hops: List[Hop], connect_timeout: float = 12.0,
                 on_failure: str = "fail"):
        self.hops = hops
        self.connect_timeout = connect_timeout
        self.on_failure = on_failure

    # -- construction ------------------------------------------------------ #

    @classmethod
    def from_config(cls, chain_cfg: Dict[str, Any]) -> "Chain":
        hops = [Hop.from_config(h, i) for i, h in enumerate(chain_cfg.get("hops") or [])]
        return cls(hops, float(chain_cfg.get("connect_timeout") or 12.0),
                   str(chain_cfg.get("on_hop_failure") or "fail"))

    @property
    def depth(self) -> int:
        return len(self.hops)

    def endpoints(self) -> List[Tuple[str, int]]:
        """Public hop addresses the firewall must let through."""
        return [(h.host, h.port) for h in self.hops]

    # -- dialling ---------------------------------------------------------- #

    async def dial(self, host: str, port: int) -> Tuple[asyncio.StreamReader, asyncio.StreamWriter]:
        if not self.hops:
            return await open_marked_connection(host, port, self.connect_timeout)
        hops = list(self.hops)
        if self.on_failure == "skip":
            while hops:
                try:
                    return await self._dial_via(hops, host, port)
                except (OSError, asyncio.TimeoutError, socks5.Socks5Error, HopError) as exc:
                    LOG.warning("hop %s failed (%s); skipping it", hops[-1].label, exc)
                    hops.pop()
            raise HopError("every hop in the chain failed")
        return await self._dial_via(hops, host, port)

    async def _dial_via(self, hops: List[Hop], host: str, port: int
                        ) -> Tuple[asyncio.StreamReader, asyncio.StreamWriter]:
        first = hops[0]
        reader, writer = await open_marked_connection(first.host, first.port,
                                                      self.connect_timeout)
        try:
            for index, hop in enumerate(hops):
                if index + 1 < len(hops):
                    target_host, target_port = hops[index + 1].host, hops[index + 1].port
                else:
                    target_host, target_port = host, port
                await asyncio.wait_for(
                    self._negotiate(hop, reader, writer, target_host, target_port),
                    timeout=self.connect_timeout,
                )
        except BaseException:
            writer.close()
            raise
        return reader, writer

    @staticmethod
    async def _negotiate(hop: Hop, reader: asyncio.StreamReader,
                         writer: asyncio.StreamWriter, host: str, port: int) -> None:
        if hop.kind == "socks5":
            await socks5.client_handshake(reader, writer, host, port,
                                          hop.username, hop.password)
        else:
            await http_connect(reader, writer, host, port, hop.username, hop.password)

    async def probe_hop(self, index: int, canary_host: str = "1.1.1.1",
                        canary_port: int = 443) -> float:
        """Reach hop *index* through the preceding hops and verify its greeting."""
        hop = self.hops[index]
        started = time.monotonic()
        if index == 0:
            reader, writer = await open_marked_connection(hop.host, hop.port,
                                                          self.connect_timeout)
            try:
                await asyncio.wait_for(self._probe(hop, reader, writer, canary_host, canary_port),
                                       timeout=self.connect_timeout)
            finally:
                writer.close()
            return time.monotonic() - started

        prefix = self.hops[:index]
        reader, writer = await self._dial_via(prefix, hop.host, hop.port)
        try:
            await asyncio.wait_for(self._probe(hop, reader, writer, canary_host, canary_port),
                                   timeout=self.connect_timeout)
        finally:
            writer.close()
        return time.monotonic() - started

    @staticmethod
    async def _probe(hop: Hop, reader: asyncio.StreamReader,
                     writer: asyncio.StreamWriter, host: str, port: int) -> None:
        if hop.kind == "socks5":
            writer.write(b"\x05\x01\x00" if hop.username is None else b"\x05\x01\x02")
            await writer.drain()
            version, method = await reader.readexactly(2)
            if version != 0x05 or method == 0xFF:
                raise HopError("hop refused SOCKS5 negotiation")
        else:
            writer.write(f"CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}\r\n\r\n".encode())
            await writer.drain()
            head = await reader.readuntil(b"\r\n\r\n")
            if not head.startswith(b"HTTP/1."):
                raise HopError("hop is not an HTTP proxy")
            code = int(head.split(b" ")[1])
            if code >= 500:
                raise HopError(f"HTTP proxy returned {code}")

    async def health(self, url: str, timeout: float = 15.0) -> Dict[str, Any]:
        started = time.monotonic()
        try:
            info = await httpc.ip_echo(self, url, timeout=timeout)
            return {
                "ok": True,
                "latency_ms": int((time.monotonic() - started) * 1000),
                "exit_ip": info.get("ip"),
                "country": info.get("country"),
            }
        except Exception as exc:  # noqa: BLE001 - reported, not swallowed
            return {
                "ok": False,
                "error": f"{type(exc).__name__}: {exc}",
                "latency_ms": int((time.monotonic() - started) * 1000),
            }


async def http_connect(reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                       host: str, port: int, username: Optional[str] = None,
                       password: Optional[str] = None) -> None:
    authority = util.join_hostport(host, port)
    lines = [f"CONNECT {authority} HTTP/1.1", f"Host: {authority}"]
    if username is not None:
        import base64

        token = base64.b64encode(f"{username}:{password or ''}".encode()).decode()
        lines.append(f"Proxy-Authorization: Basic {token}")
    writer.write(("\r\n".join(lines) + "\r\n\r\n").encode())
    await writer.drain()
    head = await reader.readuntil(b"\r\n\r\n")
    if not head.startswith(b"HTTP/1."):
        raise HopError("upstream is not an HTTP proxy")
    code = int(head.split(b" ")[1])
    if code != 200:
        raise HopError(f"HTTP CONNECT rejected with {code}")


# --------------------------------------------------------------------------- #
# routing rules
# --------------------------------------------------------------------------- #


class Router:
    """Decide chain / direct / block per destination host."""

    def __init__(self, rules: List[Dict[str, str]]):
        self.rules = list(rules or [])

    def action_for(self, host: str) -> str:
        host = host.lower().strip(".")
        for rule in self.rules:
            if match_domain(rule["domain"], host):
                return rule["action"]
        return "chain"

    @property
    def has_rules(self) -> bool:
        return bool(self.rules)


def match_domain(pattern: str, host: str) -> bool:
    pattern = pattern.lower().strip(".")
    if not pattern or not host:
        return False
    if pattern == "*":
        return True
    if "*" in pattern or "?" in pattern:
        # "*.example.com" is a subdomain rule; a further wildcard means glob.
        if pattern.startswith("*.") and "*" not in pattern[2:] and "?" not in pattern[2:]:
            suffix = pattern[2:]
            return host == suffix or host.endswith("." + suffix)
        return fnmatch.fnmatchcase(host, pattern)
    # A bare domain also covers its subdomains: example.com matches api.example.com
    return host == pattern or host.endswith("." + pattern)
