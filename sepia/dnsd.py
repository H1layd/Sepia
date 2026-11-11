"""DNS that cannot leak: a local resolver whose upstream is DoH over the chain.

There is no plaintext DNS path at all. Names are resolved by the DoH server on
the far side of the proxy chain, so neither the local network nor the ISP sees
a single query.
"""
from __future__ import annotations

import asyncio
import struct
import time
from typing import Dict, List, Optional, Tuple

from . import chain as chain_mod
from . import dnsmsg, httpc, util

LOG = util.get_logger("dns")

DOH_CONTENT_TYPE = "application/dns-message"


class CacheEntry:
    __slots__ = ("response", "expires", "stored")

    def __init__(self, response: bytes, ttl: int):
        self.response = response
        self.stored = time.time()
        self.expires = self.stored + max(0, ttl)


class Resolver:
    def __init__(self, cfg: Dict[str, object], chain: chain_mod.Chain):
        self.chain = chain
        self.servers: List[Dict[str, object]] = list(cfg.get("servers") or [])
        self.cache: Dict[str, CacheEntry] = {}
        self.block: List[str] = list(cfg.get("block") or [])
        self.block_response = str(cfg.get("block_response") or "0.0.0.0")
        self.ttl_max = int(cfg.get("cache_ttl_max") or 3600)
        self.negative_ttl = int(cfg.get("cache_negative_ttl") or 30)
        self.passthrough_local = bool(cfg.get("passthrough_local"))
        self.stats = {
            "queries": 0, "cache_hits": 0, "blocked": 0,
            "upstream_errors": 0, "upstream_calls": 0,
        }

    # -- blocklist --------------------------------------------------------- #

    def is_blocked(self, name: str) -> bool:
        lowered = name.lower().rstrip(".")
        for pattern in self.block:
            if chain_mod.match_domain(pattern, lowered):
                return True
        return False

    # -- upstream ---------------------------------------------------------- #

    async def _doh_request(self, server: Dict[str, object], wire: bytes,
                           timeout: float) -> bytes:
        url = str(server["url"])
        bootstraps = list(server.get("bootstraps") or [])
        host = url.split("://", 1)[1].split("/", 1)[0]
        if ":" in host:
            host = host.split(":")[0]
        path = "/" + url.split("://", 1)[1].split("/", 1)[1] if "/" in url.split("://", 1)[1] else "/"

        last_error: Optional[BaseException] = None
        # Never resolve the DoH endpoint by name: that name would be answered by
        # this very resolver, i.e. a deadlock. Pinned IPs only.
        attempts = bootstraps if bootstraps else [None]
        for pinned in attempts:
            try:
                return await self._doh_once(host, path, pinned, wire, timeout)
            except Exception as exc:  # noqa: BLE001 - try the next bootstrap
                last_error = exc
                LOG.debug("DoH %s via %s failed: %s", host, pinned or "system", exc)
        raise RuntimeError(f"all DoH transports failed: {last_error}")

    async def _doh_once(self, host: str, path: str, pinned: Optional[str],
                        wire: bytes, timeout: float) -> bytes:
        target = pinned or host
        reader, writer = await httpc.open_tls(self.chain, target, 443,
                                              server_hostname=host, timeout=timeout)
        try:
            headers = {
                "Host": host,
                "Content-Type": DOH_CONTENT_TYPE,
                "Accept": DOH_CONTENT_TYPE,
                "Content-Length": str(len(wire)),
                "Connection": "close",
                "User-Agent": "",
            }
            head = [f"POST {path} HTTP/1.1"] + [f"{k}: {v}" for k, v in headers.items()]
            writer.write(("\r\n".join(head) + "\r\n\r\n").encode() + wire)
            await writer.drain()
            status, _reason, resp_headers = await httpc._read_headers(reader)
            if status != 200:
                raise RuntimeError(f"DoH HTTP {status}")
            if not resp_headers.get("content-type", "").startswith(DOH_CONTENT_TYPE):
                raise RuntimeError(f"unexpected DoH content-type {resp_headers.get('content-type')!r}")
            return await httpc._read_body(reader, status, resp_headers)
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass

    # -- resolution -------------------------------------------------------- #

    async def resolve(self, wire: bytes, timeout: float = 10.0) -> bytes:
        self.stats["queries"] += 1
        try:
            query = dnsmsg.parse_query(wire)
        except dnsmsg.DnsError as exc:
            LOG.warning("malformed DNS query: %s", exc)
            return b""

        name = str(query["name"])
        qtype = int(query["qtype"])
        key = dnsmsg.question_key(name, qtype, int(query["qclass"]))

        if self.is_blocked(name):
            self.stats["blocked"] += 1
            LOG.debug("blocked %s (%s)", name, dnsmsg.TYPE_NAMES.get(qtype, qtype))
            if self.block_response in ("0.0.0.0", "::", "127.0.0.1", "::1"):
                return dnsmsg.build_address_response(query, self.block_response, ttl=60)
            return dnsmsg.build_error(query, dnsmsg.RCODE_NXDOMAIN)

        cached = self.cache.get(key)
        if cached is not None and cached.expires > time.time():
            self.stats["cache_hits"] += 1
            return _patch_id(cached.response, int(query["id"]))
        if cached is not None:
            self.cache.pop(key, None)

        last_error: Optional[BaseException] = None
        for server in self.servers:
            for attempt in range(2):
                try:
                    self.stats["upstream_calls"] += 1
                    response = await asyncio.wait_for(
                        self._doh_request(server, wire, timeout), timeout=timeout + 1
                    )
                    if not response or dnsmsg.rcode_of(response) not in (
                        dnsmsg.RCODE_NOERROR, dnsmsg.RCODE_NXDOMAIN, dnsmsg.RCODE_REFUSED
                    ):
                        raise RuntimeError(
                            f"DoH rcode {dnsmsg.rcode_of(response)} from {server['url']}")
                    self._store(key, response)
                    return response
                except Exception as exc:  # noqa: BLE001 - next server
                    last_error = exc
                    if attempt == 0:
                        continue
                    LOG.warning("DoH %s failed: %s", server.get("url"), exc)
        self.stats["upstream_errors"] += 1
        LOG.error("all DoH upstreams failed for %s: %s", name, last_error)
        return dnsmsg.build_error(query, dnsmsg.RCODE_SERVFAIL)

    def _store(self, key: str, response: bytes) -> None:
        ttl = dnsmsg.scan_ttl(response)
        if dnsmsg.rcode_of(response) == dnsmsg.RCODE_NOERROR and ttl <= 0:
            return
        if dnsmsg.rcode_of(response) != dnsmsg.RCODE_NOERROR:
            ttl = self.negative_ttl
        ttl = max(1, min(ttl, self.ttl_max))
        self.cache[key] = CacheEntry(response, ttl)
        if len(self.cache) > 5000:
            self._evict()

    def _evict(self) -> None:
        moment = time.time()
        dead = [k for k, v in self.cache.items() if v.expires <= moment]
        for k in dead:
            self.cache.pop(k, None)
        if len(self.cache) > 5000:
            ordered = sorted(self.cache.items(), key=lambda kv: kv[1].expires)
            for k, _v in ordered[: len(self.cache) - 4000]:
                self.cache.pop(k, None)

    def cache_size(self) -> int:
        return len(self.cache)

    def snapshot(self) -> Dict[str, object]:
        return {
            "servers": [s.get("url") for s in self.servers],
            "cache_entries": self.cache_size(),
            "blocked_names": len(self.block),
            **self.stats,
        }


def _patch_id(response: bytes, ident: int) -> bytes:
    if len(response) < 2:
        return response
    return struct.pack("!H", ident) + response[2:]


# --------------------------------------------------------------------------- #
# servers
# --------------------------------------------------------------------------- #


class DnsServer:
    def __init__(self, host: str, port: int, resolver: Resolver):
        self.host = host
        self.port = port
        self.resolver = resolver
        self._udp: Optional[asyncio.DatagramProtocol] = None
        self._tcp: Optional[asyncio.AbstractServer] = None
        self.requests = 0
        self.errors = 0

    async def start(self) -> None:
        loop = asyncio.get_running_loop()
        self._udp = _UdpProtocol(self)
        await loop.create_datagram_endpoint(
            lambda: self._udp, local_addr=(self.host, self.port)
        )
        self._tcp = await asyncio.start_server(self._handle_tcp, self.host, self.port)
        LOG.info("DNS resolver listening on udp+tcp %s", util.join_hostport(self.host, self.port))

    async def stop(self) -> None:
        if self._udp is not None:
            self._udp.transport.close()
            self._udp = None
        if self._tcp is not None:
            self._tcp.close()
            await self._tcp.wait_closed()
            self._tcp = None

    async def _handle_tcp(self, reader: asyncio.StreamReader,
                          writer: asyncio.StreamWriter) -> None:
        try:
            while True:
                try:
                    length_raw = await asyncio.wait_for(reader.readexactly(2), timeout=10)
                except (asyncio.IncompleteReadError, asyncio.TimeoutError):
                    break
                length = struct.unpack("!H", length_raw)[0]
                if length == 0:
                    break
                wire = await reader.readexactly(length)
                self.requests += 1
                try:
                    response = await self.resolver.resolve(wire)
                except Exception as exc:  # noqa: BLE001
                    self.errors += 1
                    LOG.warning("DNS tcp error: %s", exc)
                    break
                if not response:
                    break
                writer.write(struct.pack("!H", len(response)) + response)
                await writer.drain()
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass


class _UdpProtocol(asyncio.DatagramProtocol):
    def __init__(self, server: DnsServer):
        self.server = server
        self.transport: Optional[asyncio.DatagramTransport] = None
        self._tasks: set = set()

    def connection_made(self, transport) -> None:  # type: ignore[override]
        self.transport = transport

    def datagram_received(self, data: bytes, addr) -> None:  # type: ignore[override]
        task = asyncio.ensure_future(self._serve(data, addr))
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    async def _serve(self, data: bytes, addr) -> None:
        self.server.requests += 1
        try:
            response = await self.server.resolver.resolve(data)
        except Exception as exc:  # noqa: BLE001
            self.server.errors += 1
            LOG.warning("DNS udp error from %s: %s", addr, exc)
            return
        if not response:
            return
        try:
            udp_size = dnsmsg.parse_query(data).get("udp_size") or 512
        except dnsmsg.DnsError:
            udp_size = 512
        udp_size = max(512, min(int(udp_size), 4096))
        if len(response) > udp_size:
            response = _truncate(response)
        if self.transport is not None:
            self.transport.sendto(response, addr)


def _truncate(response: bytes) -> bytes:
    """Keep header+question, set TC so the client retries over TCP."""
    if len(response) < 12:
        return response
    flags = struct.unpack("!H", response[2:4])[0] | dnsmsg.FLAG_TC
    qdcount = struct.unpack("!H", response[4:6])[0]
    offset = 12
    try:
        for _ in range(qdcount):
            _name, offset = dnsmsg.parse_name(response, offset)
            offset += 4
    except dnsmsg.DnsError:
        offset = len(response)
    return (struct.pack("!HHHHHH", struct.unpack("!H", response[:2])[0], flags,
                        qdcount, 0, 0, 0) + response[12:offset])
